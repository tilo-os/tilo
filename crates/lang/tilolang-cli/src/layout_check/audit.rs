//! What is wrong with one laid-out window: read off the toolkit's inspection of the frame (its
//! widgets' boxes, clips and semantics) and, for text, the widgets' own measurements.

use ui::headless::InspectionNode;
use ui::kurbo::Rect;
use ui::widget::Widget;

/// A kind of layout problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Content reaches past the box it is in, and is cut off or spills over what is next to it.
    Overflow,
    /// Text needs more room than its box gives it, so part of it is not shown.
    ClippedText,
    /// Two siblings cover each other.
    Overlap,
    /// Something a person presses or types into is smaller than the design system's minimum.
    SmallHitTarget,
    /// Content is wider than the window or its scroll view, so the page scrolls sideways.
    ScrollsSideways,
    /// A scroll view with no room on the way it scrolls: what it holds cannot be scrolled into
    /// view, so it is lost (a board lane whose height came from its content).
    LostContent,
    /// An edge between two device pixels at the display's scale, drawn blurred across both.
    OffPixel,
    /// A page whose content is one row stretched over the column: a row is a line, and the page
    /// around it holds it.
    BareRow,
    /// The rows of one box stand on different columns: their titles start at different x, or
    /// their content ends at different edges, or a nesting step does not indent by one gutter.
    RaggedColumns,
    /// Two selection shapes at once: a selected box inside another selected box, the same choice
    /// filled twice.
    DoubleSelection,
    /// A screen made for a narrow window holds its content nearer than a gutter to the edge.
    CrampedGutter,
    /// One screen offers the same way to add something several times: two add affordances in one
    /// list, or the same add label on two controls.
    SeveralAdds,
    /// A name a screen reader announces reads as a code identifier rather than as words.
    CodedName,
    /// A destructive control says so with a verb of its own, where every screen must use one.
    UnsharedVerb,
    /// A surface drawn inside another surface of the same kind: a card in a card.
    NestedCard,
    /// A page's "…" standing in a column rather than at the trailing edge of its title's line:
    /// the door takes a line of its own and shows a lone glyph under the title.
    LoneDoor,
    /// A heading with nothing in it: a band's height taken over the content, where a heading is
    /// the words its section goes by.
    EmptyHeading,
    /// A count shown as zero. A count means something wants attention, so zero is no badge at
    /// all, and no room kept for one.
    ZeroCount,
    /// A column's header drawing a count the sidebar beside it already draws for the place
    /// shown: the same number twice on one screen.
    RepeatedCount,
    /// A surface hanging over the window, a popover or a sheet, reaching past the window's
    /// edges: what it says there is cut off or cannot be pressed.
    PastWindow,
    /// A column's header row that broke the row's own rule: a title cut off while the row could
    /// have given it more (room standing empty beside it, or an action keeping a label it could
    /// show as its icon), or a row that dropped the app's primary action.
    TitleClippedWithRoom,
    /// A word the window shows from a catalogue in another language than the one it speaks: a
    /// window says everything in one language, so a German word in an English window (or the
    /// source standing in for a translation the window's own catalogues lack) is a fault.
    MixedLanguage,
    /// An item standing open where it stands, outside the list that shows it: the row that
    /// opened it is no row of a list the person sees, or more than one row of one list stands
    /// open at a time.
    OpenOutsideList,
    /// A surface with nothing in it: a filled block standing where a person reads it as a card,
    /// a page or a band, and holding no words and nothing laid out of its own.
    EmptySurface,
    /// A column's header naming the page while the page shows the same words as its own
    /// heading, or in the field it edits its title in: one title on screen, and the page's.
    RepeatedPageTitle,
    /// A back control standing beside the sidebar toggle in the same bar row: one corner
    /// offering two ways to navigate, where a screen that went somewhere deeper has one, its
    /// way back at the leading edge.
    DoubleNavigation,
    /// A list or tree whose first level starts more than one nesting step right of the title its
    /// section goes by: a section's title stands on the column its content does, and a first
    /// level keeps no more than the one step a folding chevron hangs in front of it.
    FirstLevelIndent,
    /// One bar showing one control twice: the same action under two ids, the same icon saying
    /// the same words twice, or a second "…" beside the bar's own. One bar shows each control
    /// once, and has one door to what does not fit in it.
    DuplicateBarControl,
    /// An action that asks for its words standing as its icon alone in a bar with room for those
    /// words: a glyph nobody can name, where the bar could have said what it does.
    IconOnlyBarAction,
    /// A pane that only says nothing is chosen, taking the window beside the list it is chosen
    /// from: with nothing chosen the list lists for the whole window, and the pane appears when
    /// something opens in it.
    PlaceholderPane,
    /// A selection lift a list painted across the space around what its row shows: the person's
    /// choice filled edge to edge, gutters and all, where a row paints it around its content.
    SelectionBleed,
}

impl Kind {
    pub const ALL: [Self; 31] = [
        Self::Overflow,
        Self::ClippedText,
        Self::Overlap,
        Self::SmallHitTarget,
        Self::ScrollsSideways,
        Self::LostContent,
        Self::OffPixel,
        Self::BareRow,
        Self::RaggedColumns,
        Self::DoubleSelection,
        Self::CrampedGutter,
        Self::SeveralAdds,
        Self::CodedName,
        Self::UnsharedVerb,
        Self::NestedCard,
        Self::LoneDoor,
        Self::EmptyHeading,
        Self::ZeroCount,
        Self::RepeatedCount,
        Self::PastWindow,
        Self::TitleClippedWithRoom,
        Self::MixedLanguage,
        Self::OpenOutsideList,
        Self::EmptySurface,
        Self::RepeatedPageTitle,
        Self::DoubleNavigation,
        Self::FirstLevelIndent,
        Self::DuplicateBarControl,
        Self::IconOnlyBarAction,
        Self::PlaceholderPane,
        Self::SelectionBleed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::ClippedText => "clipped-text",
            Self::Overlap => "overlap",
            Self::SmallHitTarget => "small-hit-target",
            Self::ScrollsSideways => "scrolls-sideways",
            Self::LostContent => "lost-content",
            Self::OffPixel => "off-pixel",
            Self::BareRow => "bare-row",
            Self::RaggedColumns => "ragged-columns",
            Self::DoubleSelection => "double-selection",
            Self::CrampedGutter => "cramped-gutter",
            Self::SeveralAdds => "several-adds",
            Self::CodedName => "coded-name",
            Self::UnsharedVerb => "unshared-verb",
            Self::NestedCard => "nested-card",
            Self::LoneDoor => "lone-door",
            Self::EmptyHeading => "empty-heading",
            Self::ZeroCount => "zero-count",
            Self::RepeatedCount => "repeated-count",
            Self::PastWindow => "past-window",
            Self::TitleClippedWithRoom => "title-clipped-with-room",
            Self::MixedLanguage => "mixed-language",
            Self::OpenOutsideList => "open-outside-list",
            Self::EmptySurface => "empty-surface",
            Self::RepeatedPageTitle => "repeated-page-title",
            Self::DoubleNavigation => "double-navigation",
            Self::FirstLevelIndent => "first-level-indent",
            Self::DuplicateBarControl => "duplicate-bar-control",
            Self::IconOnlyBarAction => "icon-only-bar-action",
            Self::PlaceholderPane => "placeholder-pane",
            Self::SelectionBleed => "selection-bleed",
        }
    }
}

/// One problem in one frame, at the widget it is about.
#[derive(Clone, Debug)]
pub struct Problem {
    pub kind: Kind,
    /// The keys of the widgets at and above it that a view's nodes made, innermost first.
    pub keys: Vec<String>,
    /// The toolkit widget it is about.
    pub widget: String,
    /// That widget's own test id, if it has one.
    pub id: Option<String>,
    /// What is wrong, in logical pixels.
    pub detail: String,
}

/// Slack for rounding: layout places edges on fractions of a pixel.
const EPSILON: f64 = 1.0;

/// How far from a device pixel an edge may stand, in device pixels: what the toolkit's single
/// precision coordinates carry, far under anything a person could see.
const PIXEL_SLACK: f64 = 0.01;

/// The tail of a page's "…" test id: the door the host gives a page's title the actions for its
/// item behind.
const DOOR: &str = "~more";

/// A line's widget: its children side by side, where a door belongs at the trailing edge.
const LINE: &str = "Row";

/// A heading's widget: the band a section goes by, over what the section holds.
const HEADING: &str = "SectionHeader";

/// The shell's way back, and its sidebar toggle: the two controls a bar navigates with.
const BACK: &str = "nav-shell-back";
const SIDEBAR_TOGGLE: &str = "nav-shell-sidebar";

/// The window shell, whose columns stand as its own children.
const SHELL: &str = "NavShell";

/// The pane a window keeps for what is not chosen yet.
const CHOICE_PANE: &str = "EmptyState";

/// The share of the window a pane takes to stand for a column rather than to say one notice: the
/// width and the height its content column keeps while something is open in it.
const PANE_WIDTH: f64 = 0.25;
const PANE_HEIGHT: f64 = 0.5;

fn rect(r: [f64; 4]) -> Rect {
    Rect::new(r[0], r[1], r[0] + r[2], r[1] + r[3])
}

fn has_area(r: Rect) -> bool {
    r.width() > EPSILON && r.height() > EPSILON
}

/// Whether a box, or anything it holds, gives a screen reader words to announce.
fn has_words(node: &InspectionNode) -> bool {
    node.semantics
        .as_ref()
        .and_then(|access| access.name.as_deref())
        .is_some_and(|name| !name.trim().is_empty())
        || node.children.iter().any(has_words)
}

fn size(r: Rect) -> String {
    format!("{:.0}×{:.0}", r.width(), r.height())
}

/// `text` as one line of words: whitespace collapsed, ends trimmed.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The words a box gives a screen reader: its own, or the first of what it holds.
fn words_of(node: &InspectionNode) -> String {
    node.semantics
        .as_ref()
        .and_then(|access| access.name.as_deref())
        .map(words)
        .filter(|said| !said.is_empty())
        .or_else(|| {
            node.children
                .iter()
                .map(words_of)
                .find(|said| !said.is_empty())
        })
        .unwrap_or_default()
}

/// Which ways a box lets its content go on past it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Scrolls {
    /// Content may go on past its bottom (a column's body, a list).
    down: bool,
    /// Content may go on past its sides (a board's lanes in `ui.Scroll(across: true)`).
    across: bool,
}

/// Which ways `node` scrolls its content: as a scroll view says of its axis, else down for a
/// widget that scrolls by its kind (a long list).
fn scrolls(node: &InspectionNode) -> Scrolls {
    use ui::widget::{LayoutInfo, ScrollDirection};
    if let Some(LayoutInfo::Scroll(scroll)) = &node.layout {
        return match scroll.direction {
            ScrollDirection::Vertical => Scrolls {
                down: true,
                across: false,
            },
            ScrollDirection::Horizontal => Scrolls {
                down: false,
                across: true,
            },
            ScrollDirection::Both => Scrolls {
                down: true,
                across: true,
            },
        };
    }
    let lower = node.widget.to_ascii_lowercase();
    // A navigation list scrolls its rows on the same list engine as a virtual list
    // (`ui_core::widget::nav_list`): a sidebar with more places than fit scrolls, it does not
    // overflow.
    let down = lower.contains("scroll")
        || lower.contains("virtuallist")
        || lower.contains("describedlist")
        || lower.contains("navlist");
    Scrolls {
        down,
        across: false,
    }
}

/// Whether a node is one row of a list: what a list draws per item, which a surface of its own
/// (a result, a notice) draws too.
fn is_row(node: &InspectionNode) -> bool {
    matches!(
        node.widget.as_str(),
        "ListRow" | "TreeViewRow" | "SwipeRow" | "ListItem"
    ) || node
        .semantics
        .as_ref()
        .is_some_and(|access| access.role == ui::a11y::Role::ListItem)
}

/// Whether a node is one row of a list of items: what a list draws per item. The rows of a tree
/// are the branches it opens, not items a window lists beside its content.
fn is_item_row(node: &InspectionNode) -> bool {
    is_row(node) && node.widget != "TreeViewRow"
}

/// Whether `node` shows one of the rows `pane` stands beside, in the window `window`: beside is
/// side by side, both of them on screen, one over the other's height.
fn beside_rows(pane: Rect, node: &InspectionNode, window: Rect) -> bool {
    if is_item_row(node) {
        let row = rect(node.rect).intersect(window);
        return has_area(row)
            && (row.x1 <= pane.x0 + EPSILON || row.x0 >= pane.x1 - EPSILON)
            && row.y0 < pane.y1 - EPSILON
            && row.y1 > pane.y0 + EPSILON;
    }
    node.children
        .iter()
        .any(|child| beside_rows(pane, child, window))
}

/// Whether a pane takes the room of a column: more of the window than a notice standing in what
/// it says.
fn pane_sized(pane: Rect, window: Rect) -> bool {
    has_area(pane)
        && pane.width() >= window.width() * PANE_WIDTH
        && pane.height() >= window.height() * PANE_HEIGHT
}

/// Whether a widget hangs part of itself outside the column it is in by design: a document's
/// block editor hangs its handle gutter in the margin, so its text lines up with the title
/// above it.
fn hangs(widget: &str) -> bool {
    matches!(widget, "BlockEditor")
}

/// Whether a widget floats over the content it belongs to, shown only on hover or focus (a
/// row's hover toolbar over its trailing top corner): neither overflow nor overlap.
fn floats(widget: &str) -> bool {
    matches!(widget, "HoverToolbarFace")
}

/// Whether a widget the window's top layer paints: it stands against the window it floats in,
/// not the box that opened it, so it neither widens what its owner holds nor reaches past it.
fn floats_in_window(widget: &str) -> bool {
    matches!(widget, "ContextMenu" | "Tooltip" | "Popover")
}

/// The boxes a widget holds: what it shows inside itself. What the window's top layer paints
/// over it (a menu its rows opened) is not one of them.
fn held(node: &InspectionNode) -> Option<Rect> {
    node.children
        .iter()
        .filter(|child| !floats(&child.widget) && !floats_in_window(&child.widget))
        .map(|child| rect(child.rect))
        .filter(|r| has_area(*r))
        .reduce(|a, b| a.union(b))
}

/// Whether a node is text itself.
fn is_text(node: &InspectionNode) -> bool {
    use ui::a11y::Role;
    node.widget == "Text"
        || node.semantics.as_ref().is_some_and(|access| {
            matches!(
                access.role,
                Role::StaticText | Role::Label | Role::Heading { .. }
            )
        })
}

/// The boxes of the text `node` shows, its own and its descendants', each as much of it as can
/// be seen: what a scroller or a clip of theirs hides is not read, so it covers nothing, and
/// what floats over everything (a menu, a tooltip) is no text of the box it opens from.
fn text_boxes(node: &InspectionNode, clip: Rect, out: &mut Vec<Rect>) {
    if floats(&node.widget) {
        return;
    }
    if is_text(node) {
        let seen = rect(node.rect).intersect(clip);
        if has_area(seen) {
            out.push(seen);
        }
        return;
    }
    let inner = node
        .child_clip
        .map_or(clip, |child_clip| clip.intersect(rect(child_clip)));
    for child in &node.children {
        text_boxes(child, inner, out);
    }
}

/// Whether some text of `a` and some text of `b` stand on each other, so neither can be read:
/// two boxes laid over each other on purpose (a dock over the desk under the bar) whose text
/// stands apart can both be read.
fn texts_meet(a: &InspectionNode, b: &InspectionNode, clip: Rect) -> bool {
    let (mut under, mut over) = (Vec::new(), Vec::new());
    text_boxes(a, clip, &mut under);
    text_boxes(b, clip, &mut over);
    under
        .iter()
        .any(|u| over.iter().any(|o| has_area(u.intersect(*o))))
}

/// Whether `node` is a window standing on a desktop, or holds only one: its surface is opaque
/// and drawn over what it stands on, so what it covers is behind it by design, as on every
/// desktop, and nothing of the two is read through the other.
fn stands_as_window(node: &InspectionNode) -> bool {
    node.widget == "WindowFrame" || matches!(&node.children[..], [only] if stands_as_window(only))
}

/// Whether a widget lays its children over each other on purpose.
fn layers(widget: &str) -> bool {
    matches!(
        widget,
        "Layers"
            | "NavShell"
            | "WindowFrame"
            | "NavPane"
            | "Pane"
            | "TilolangWindow"
            | "Decorated"
            | "Toast"
            | "Banner"
    ) || widget.contains("Overlay")
        || widget.contains("Layer")
}

/// Whether a widget paints a surface of its own: a ground or a card a person reads as a block,
/// which shows what it holds. A block with nothing in it says there is something here when
/// there is not. A well is no such block: it is the room another program's window draws in, so
/// it holds what this tree cannot see.
fn paints_surface(widget: &str) -> bool {
    matches!(
        widget,
        "Banner"
            | "Card"
            | "CardGrid"
            | "Docked"
            | "EmptyState"
            | "FloatingPanel"
            | "FormSection"
            | "Page"
            | "PageSidebar"
            | "Section"
    )
}

/// Whether a node is one control a person presses: its own role says so, or it is a box (a
/// header action's frame) around exactly one.
fn is_control(node: &InspectionNode) -> bool {
    if node
        .semantics
        .as_ref()
        .is_some_and(|access| interactive(access.role) && !access.state.disabled)
    {
        return true;
    }
    match node.children.as_slice() {
        [only] => is_control(only),
        _ => false,
    }
}

/// Roles a person presses, picks or types into.
fn interactive(role: ui::a11y::Role) -> bool {
    use ui::a11y::Role;
    matches!(
        role,
        Role::Button
            | Role::ToggleButton
            | Role::Checkbox
            | Role::RadioButton
            | Role::Switch
            | Role::Slider
            | Role::SpinButton
            | Role::ComboBox
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::Link
            | Role::Tab
            | Role::MenuItem
            | Role::ListBoxOption
            | Role::TreeItem
    )
}

struct Walk<'a> {
    viewport: Rect,
    /// Inside a box already found reaching past the window's sides.
    beyond: bool,
    /// Inside a view that scrolls sideways: its content reaches past the window by design.
    across: bool,
    /// The smallest a target may be, where the face is the target (on touch).
    min_target: Option<f64>,
    /// Device pixels per logical pixel.
    display_scale: f64,
    /// Inside a box already found off the device pixels: what it holds moves with it.
    off_pixel: bool,
    /// The counts the window's column headers draw.
    header_counts: Vec<Count>,
    /// The counts a window's places draw beside the place chosen in them.
    place_counts: Vec<Count>,
    /// The titles the window's column headers draw.
    header_titles: Vec<Title>,
    /// The titles the window's content shows as its own.
    page_titles: Vec<Title>,
    /// The shell's own navigation controls, where the window shows them.
    navigations: Vec<Navigation>,
    out: &'a mut Vec<Problem>,
}

/// Words a window shows as a title: a column header's, or one its content shows as the page's.
struct Title {
    words: String,
    at: Rect,
    /// The keys of the widgets at and above it, innermost first.
    keys: Vec<String>,
    widget: String,
    id: Option<String>,
}

/// One number a window draws: a column header's count, or a place's.
struct Count {
    number: u32,
    at: Rect,
    /// The keys of the widgets at and above it, innermost first.
    keys: Vec<String>,
    widget: String,
    id: Option<String>,
}

/// A navigation control the window's bar shows: the way back, or the sidebar's toggle.
struct Navigation {
    /// Whether it is the way back rather than the sidebar's toggle.
    back: bool,
    at: Rect,
    /// The keys of the widgets at and above it, innermost first.
    keys: Vec<String>,
    widget: String,
    id: Option<String>,
}

/// Every problem of the frame `root`, laid out in `viewport`, where a target needs `min_target`
/// logical pixels on each side. On a fine pointer the toolkit grows each control's hit area
/// about its centre to the minimum, so targets are checked where the face itself must be the
/// target: on touch, where faces grow to it. At `display_scale` device pixels per logical
/// pixel, every edge must stand on a device pixel.
pub fn inspect(
    root: &InspectionNode,
    viewport: (u32, u32),
    min_target: Option<f64>,
    display_scale: f64,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let viewport = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
    let mut walk = Walk {
        viewport,
        beyond: false,
        across: false,
        min_target,
        display_scale,
        off_pixel: false,
        header_counts: Vec::new(),
        place_counts: Vec::new(),
        header_titles: Vec::new(),
        page_titles: Vec::new(),
        navigations: Vec::new(),
        out: &mut out,
    };
    walk.node(
        root,
        &mut Vec::new(),
        viewport,
        viewport,
        Scrolls::default(),
        false,
        "",
    );
    walk.repeated_counts();
    walk.repeated_titles();
    walk.double_navigation();
    walk.placeholder_panes(root, &mut Vec::new());
    out
}

impl Walk<'_> {
    /// `parent`: the nearest box above with geometry. `clip`: what of the window the node can show in. `scroller`:
    /// which ways the box above lets content go on past it. `content`: the box is a scroll view's, holding
    /// what the view scrolls through rather than being a container of its own. `above`: the widget the node
    /// stands in, which decides whether it stands on a line.
    fn node(
        &mut self,
        node: &InspectionNode,
        keys: &mut Vec<String>,
        parent: Rect,
        clip: Rect,
        scroller: Scrolls,
        content: bool,
        above: &str,
    ) {
        let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
        self.visit(node, keys, parent, clip, scroller, content, above);
        if pushed {
            keys.pop();
        }
    }

    fn visit(
        &mut self,
        node: &InspectionNode,
        keys: &mut Vec<String>,
        parent: Rect,
        clip: Rect,
        scroller: Scrolls,
        content: bool,
        above: &str,
    ) {
        let own = rect(node.rect);
        let geometric = has_area(own);
        let scrolls = scrolls(node);
        let scrolling = scrolls.down || scrolls.across;
        // The box a scroll view holds its content in is that view, not a container of its own:
        // what it holds goes on past the view as the view allows, and what that content holds in
        // turn is checked against the content, not the view.
        let content = content && geometric && !scrolling;
        // What of the window is not shown at all needs no checking: a column pushed aside, a
        // row scrolled out of view.
        if geometric && !has_area(own.intersect(clip)) {
            return;
        }
        let was_beyond = self.beyond;
        let was_across = self.across;
        let was_off_pixel = self.off_pixel;
        if geometric {
            self.pixels(node, keys, own);
            self.bare_row(node, keys, own);
            self.lone_door(node, keys, above);
            self.empty_heading(node, keys, own);
            self.empty_surface(node, keys, own);
        }
        if geometric && !hangs(&node.widget) {
            self.reach(node, keys, own, parent, scroller);
            if let Some(min) = self.min_target {
                self.target(node, keys, own, min);
            }
        }
        // The box children are measured against, and which ways it scrolls them.
        // A manual region in a view that pans both ways is the canvas's content placed by code
        // (its places, put where its camera puts them): what it places pans as the canvas does.
        // In a view that scrolls one way a region sizes itself, and its children are checked
        // against it.
        let placed_content = matches!(node.layout, Some(ui::widget::LayoutInfo::Manual(_)))
            && scroller.down
            && scroller.across;
        let (inner_parent, inner_scroller) = if geometric && placed_content {
            (own, scroller)
        } else if content {
            (parent, scroller)
        } else if geometric {
            (own, scrolls)
        } else {
            (parent, scroller)
        };
        if scrolls.across && geometric {
            self.across = true;
        }
        // A scroll view shows its content within its own box, whether or not it reports a clip.
        let mut inner_clip = clip;
        if let Some(child_clip) = node.child_clip {
            inner_clip = clip.intersect(rect(child_clip));
        } else if scrolling && geometric {
            inner_clip = clip.intersect(own);
        }
        // A view that scrolls sideways holds wider content by design; one that scrolls only down
        // and holds it scrolls the page sideways.
        if scrolling && !scrolls.across && geometric {
            self.sideways(node, keys, node.child_clip.map_or(own, rect));
        }
        if scrolling {
            self.lost(node, keys, own);
        }
        if geometric {
            self.overlaps(node, keys, inner_clip);
            self.counts(node, keys, own, inner_clip);
            self.titles(node, keys, own);
            self.header_rows(node, keys);
            self.navigation(node, keys, own.intersect(inner_clip));
        }
        for child in &node.children {
            if floats(&child.widget) {
                continue;
            }
            // A menu the window's top layer paints stands against the window, not the box that
            // opened it: it is measured from the window's edges, and its own edges and targets
            // are still checked.
            if floats_in_window(&child.widget) {
                self.past_window(child, keys);
                let (beyond, across, off_pixel) = (self.beyond, self.across, self.off_pixel);
                (self.beyond, self.across, self.off_pixel) = (false, false, false);
                self.node(
                    child,
                    keys,
                    self.viewport,
                    self.viewport,
                    Scrolls::default(),
                    false,
                    &node.widget,
                );
                (self.beyond, self.across, self.off_pixel) = (beyond, across, off_pixel);
                continue;
            }
            // A scroll view holds its content in a box of its own, which is the view's own
            // content rather than a frame around it.
            self.node(
                child,
                keys,
                inner_parent,
                inner_clip,
                inner_scroller,
                scrolling,
                &node.widget,
            );
        }
        self.beyond = was_beyond;
        self.across = was_across;
        self.off_pixel = was_off_pixel;
    }

    /// A box with an edge between two device pixels: the outermost such box only, since what
    /// it holds is placed from its edges.
    fn pixels(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        if self.off_pixel {
            return;
        }
        let scale = self.display_scale;
        let off = |edge: f64| (edge * scale - (edge * scale).round()).abs() > PIXEL_SLACK;
        let edges = [
            ("left", own.x0),
            ("top", own.y0),
            ("right", own.x1),
            ("bottom", own.y1),
        ];
        let Some((side, edge)) = edges.into_iter().find(|(_, edge)| off(*edge)) else {
            return;
        };
        self.off_pixel = true;
        self.push(
            Kind::OffPixel,
            node,
            keys,
            format!(
                "its {side} edge at {edge:.3} falls at device pixel {:.3} at {scale}x",
                edge * scale
            ),
        );
    }

    /// A row stretched over the column it stands in: the page drawn as one row, its line
    /// floating at the column's middle where a page around it would hold it. A row is as tall
    /// as its line wherever it stands — a list's row, a result in a page, a notice's — so a row
    /// taking half the window's height with its line floating in it is the page itself. A row
    /// its content fills to the same height holds what it draws (a message in a conversation,
    /// whose body is long): that is the content's height, not the column's.
    fn bare_row(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        let window = self.viewport.height();
        if !is_row(node) || window <= 0.0 || own.height() < window * 0.5 {
            return;
        }
        let held = node
            .children
            .iter()
            .filter(|child| !floats(&child.widget))
            .map(|child| rect(child.rect).height())
            .fold(0.0_f64, f64::max);
        if held >= own.height() * 0.5 {
            return;
        }
        self.push(
            Kind::BareRow,
            node,
            keys,
            format!(
                "a {} {} fills {:.0}% of the window's height: a row is a line, and the page it stands in holds it",
                node.widget,
                size(own),
                own.height() / window * 100.0
            ),
        );
    }

    /// A page's "…" in a column: the door is the item's actions at the trailing edge of the
    /// title's line, so one standing in a column takes a line of the document for its glyph
    /// alone, under the title and out of reach of the page's own composition.
    fn lone_door(&mut self, node: &InspectionNode, keys: &[String], above: &str) {
        let Some(id) = node.test_id.as_deref() else {
            return;
        };
        if node.widget != "MenuButton" || !id.ends_with(DOOR) || above == LINE {
            return;
        }
        self.push(
            Kind::LoneDoor,
            node,
            keys,
            format!("it stands in a {above} of its own rather than at the trailing edge of the title's line"),
        );
    }

    /// A heading with nothing in it: it takes the band a section goes by, and the content it
    /// stands over starts below an empty strip. A heading is the words its section goes by;
    /// where nothing heads the content, the content starts at the page's own distance.
    fn empty_heading(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        if node.widget != HEADING || has_words(node) {
            return;
        }
        self.push(
            Kind::EmptyHeading,
            node,
            keys,
            format!("it takes {} and says nothing", size(own)),
        );
    }

    /// A surface with nothing in it: the block takes the ground and the corners of a card or a
    /// page, and no words stand in it and nothing of its own is laid out. What a person sees is a
    /// filled rectangle where a thing should be: a surface is drawn for what it holds, so one
    /// holding nothing is not drawn at all.
    fn empty_surface(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        if !paints_surface(&node.widget) || has_words(node) || held(node).is_some() {
            return;
        }
        self.push(
            Kind::EmptySurface,
            node,
            keys,
            format!("it paints {} and holds nothing", size(own)),
        );
    }

    /// A surface hanging over the window whose own box reaches past the window's edges: it is
    /// placed against the window, so it must stay inside it, whatever it holds.
    fn past_window(&mut self, node: &InspectionNode, keys: &[String]) {
        let own = rect(node.rect);
        let window = self.viewport;
        if own.x0 < window.x0 - EPSILON
            || own.x1 > window.x1 + EPSILON
            || own.y0 < window.y0 - EPSILON
            || own.y1 > window.y1 + EPSILON
        {
            self.push(
                Kind::PastWindow,
                node,
                keys,
                format!(
                    "it reaches from x {:.0} to {:.0} and y {:.0} to {:.0} in a window {} at ({:.0}, {:.0})",
                    own.x0,
                    own.x1,
                    own.y0,
                    own.y1,
                    size(window),
                    window.x0,
                    window.y0
                ),
            );
        }
    }

    /// A box past its container's, or past the window's sides.
    fn reach(
        &mut self,
        node: &InspectionNode,
        keys: &[String],
        own: Rect,
        parent: Rect,
        scroller: Scrolls,
    ) {
        let past_window =
            own.x0 < self.viewport.x0 - EPSILON || own.x1 > self.viewport.x1 + EPSILON;
        if past_window && !self.across {
            if self.beyond {
                return;
            }
            self.beyond = true;
            self.push(
                Kind::ScrollsSideways,
                node,
                keys,
                format!(
                    "{} reaches from x {:.0} to {:.0} in a window {:.0} wide",
                    size(own),
                    own.x0,
                    own.x1,
                    self.viewport.width()
                ),
            );
            return;
        }
        // In a scroll view, content goes on past it on its axis: only the other axis counts.
        let across = own.x0 < parent.x0 - EPSILON || own.x1 > parent.x1 + EPSILON;
        let along = own.y0 < parent.y0 - EPSILON || own.y1 > parent.y1 + EPSILON;
        if (across && !scroller.across) || (along && !scroller.down) {
            self.push(
                Kind::Overflow,
                node,
                keys,
                format!(
                    "{} at ({:.0}, {:.0}) reaches past its container's {} at ({:.0}, {:.0})",
                    size(own),
                    own.x0,
                    own.y0,
                    size(parent),
                    parent.x0,
                    parent.y0
                ),
            );
        }
    }

    /// A scroll view whose content is wider than it.
    fn sideways(&mut self, node: &InspectionNode, keys: &[String], clip: Rect) {
        let content = held(node);
        if let Some(content) = content
            && content.width() > clip.width() + EPSILON
        {
            self.push(
                Kind::ScrollsSideways,
                node,
                keys,
                format!(
                    "content {:.0} wide in a scroll view {:.0} wide",
                    content.width(),
                    clip.width()
                ),
            );
        }
    }

    /// A scroll view with no room on the way it scrolls, holding more than that: nothing under
    /// it can be scrolled into view, so a card or a line the person was promised is gone.
    fn lost(&mut self, node: &InspectionNode, keys: &[String], shown: Rect) {
        let content = held(node);
        let Some(content) = content else {
            return;
        };
        let scrolls = scrolls(node);
        let lost = [
            (scrolls.down, shown.height(), content.height(), "tall"),
            (scrolls.across, shown.width(), content.width(), "wide"),
        ]
        .into_iter()
        .find(|(scrolls, shown, content, _)| *scrolls && *shown <= EPSILON && *content > EPSILON);
        let Some((_, shown, content, axis)) = lost else {
            return;
        };
        self.push(
            Kind::LostContent,
            node,
            keys,
            format!(
                "content {content:.0} {axis} in a scroll view {shown:.0} {axis}, which cannot \
                 scroll it into view"
            ),
        );
    }

    /// Something to press smaller than the minimum.
    fn target(&mut self, node: &InspectionNode, keys: &[String], own: Rect, min: f64) {
        let Some(semantics) = &node.semantics else {
            return;
        };
        let pressable = interactive(semantics.role)
            || (node.focusable && semantics.role == ui::a11y::Role::ListItem);
        if pressable && !semantics.state.disabled {
            let hit = semantics
                .bounds
                .map_or(own, |bounds| own.union(bounds.to_rect()));
            self.small(node, keys, hit, min, &format!("{:?}", semantics.role));
        }
        // The parts a widget draws itself (a chip's remove button), each with its own bounds.
        for part in &semantics.children {
            if interactive(part.role)
                && !part.state.disabled
                && let Some(bounds) = part.bounds
            {
                self.small(
                    node,
                    keys,
                    bounds.to_rect(),
                    min,
                    &format!("its {:?}", part.role),
                );
            }
        }
    }

    /// The numbers `node` draws: a column header's count, or the counts of the places a list
    /// shows. Zero wants no attention, so it is no count at all and may not be drawn; the count
    /// of the place chosen in a list, drawn beside a column's header, is the same number twice.
    fn counts(&mut self, node: &InspectionNode, keys: &[String], own: Rect, clip: Rect) {
        let Some(access) = node.semantics.as_ref() else {
            return;
        };
        let number = |value: Option<&str>| value.and_then(|value| value.trim().parse::<u32>().ok());
        match node.widget.as_str() {
            "NavShellTitle" => {
                if let Some(name) = access
                    .name
                    .as_deref()
                    .map(words)
                    .filter(|name| !name.is_empty())
                {
                    self.header_titles.push(Title {
                        words: name,
                        at: own,
                        keys: keys.iter().rev().cloned().collect(),
                        widget: node.widget.clone(),
                        id: node.test_id.clone(),
                    });
                }
                let Some(count) = number(access.value.as_deref()) else {
                    return;
                };
                if count == 0 {
                    self.push(
                        Kind::ZeroCount,
                        node,
                        keys,
                        "the column's count reads 0, which shows nothing".into(),
                    );
                    return;
                }
                self.header_counts.push(Count {
                    number: count,
                    at: own,
                    keys: keys.iter().rev().cloned().collect(),
                    widget: node.widget.clone(),
                    id: node.test_id.clone(),
                });
            }
            "NavList" => {
                for place in &access.children {
                    let Some(bounds) = place.bounds.map(ui::a11y::AccessBounds::to_rect) else {
                        continue;
                    };
                    if !has_area(bounds.intersect(clip)) {
                        continue;
                    }
                    let Some(count) = number(place.value.as_deref()) else {
                        continue;
                    };
                    let mut keys = keys.to_vec();
                    keys.extend(place.semantic_key.clone());
                    if count == 0 {
                        self.push(
                            Kind::ZeroCount,
                            node,
                            &keys,
                            format!(
                                "the count beside {:?} reads 0, which shows nothing",
                                place.name
                            ),
                        );
                    } else if place.state.selected {
                        self.place_counts.push(Count {
                            number: count,
                            at: bounds,
                            keys: keys.iter().rev().cloned().collect(),
                            widget: node.widget.clone(),
                            id: node.test_id.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// A column's header row as the shell settled it: the row holds its title against its
    /// actions by one rule at every width and in every language — the actions give way first,
    /// a label for its icon and then a place in the row for the menu's door, and the row never
    /// drops the app's primary action.
    fn header_rows(&mut self, node: &InspectionNode, keys: &[String]) {
        let Some(ui::widget::LayoutInfo::Headers(rows)) = &node.layout else {
            return;
        };
        for row in rows.rows.iter().flatten() {
            if let Some(detail) = header_row_problem(row) {
                self.push(Kind::TitleClippedWithRoom, node, keys, detail);
            }
        }
    }

    /// A column's header count that a place beside it, to its left, draws too: the same number
    /// twice on one screen.
    fn repeated_counts(&mut self) {
        let (headers, places) = (
            std::mem::take(&mut self.header_counts),
            std::mem::take(&mut self.place_counts),
        );
        for header in headers {
            let Some(place) = places.iter().find(|place| {
                place.number == header.number && place.at.x1 <= header.at.x0 + EPSILON
            }) else {
                continue;
            };
            self.out.push(Problem {
                kind: Kind::RepeatedCount,
                keys: header.keys.clone(),
                widget: header.widget.clone(),
                id: header.id.clone(),
                detail: format!(
                    "the count {} is drawn again at the place {} wide, {} from it",
                    header.number,
                    size(place.at),
                    (header.at.x0 - place.at.x1).round()
                ),
            });
        }
    }

    /// The words `node` shows as a title of its own: the page's own heading, as the toolkit
    /// names it. What the person reads as the page's name, beside the bar's.
    fn titles(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        let Some(shown) = node
            .page_title
            .as_deref()
            .map(words)
            .filter(|shown| !shown.is_empty())
        else {
            return;
        };
        self.page_titles.push(Title {
            words: shown,
            at: own,
            keys: keys.iter().rev().cloned().collect(),
            widget: node.widget.clone(),
            id: node.test_id.clone(),
        });
    }

    /// A bar naming a page while the page shows those same words as its own title: the bar
    /// keeps only the way back and the page's actions until the title scrolls out of the page.
    fn repeated_titles(&mut self) {
        let (headers, pages) = (
            std::mem::take(&mut self.header_titles),
            std::mem::take(&mut self.page_titles),
        );
        for header in headers {
            let Some(page) = pages.iter().find(|page| {
                page.words.eq_ignore_ascii_case(&header.words)
                    && page.at.y0 >= header.at.y1 - EPSILON
            }) else {
                continue;
            };
            self.out.push(Problem {
                kind: Kind::RepeatedPageTitle,
                keys: header.keys.clone(),
                widget: header.widget.clone(),
                id: header.id.clone(),
                detail: format!(
                    "the bar names the page {:?} while the page shows it {} below, {} wide",
                    page.words,
                    (page.at.y0 - header.at.y1).round(),
                    size(page.at)
                ),
            });
        }
    }

    /// The shell's own navigation controls, where the window shows them: its way back, and its
    /// sidebar toggle.
    fn navigation(&mut self, node: &InspectionNode, keys: &[String], shown: Rect) {
        let Some(id) = node.test_id.as_deref() else {
            return;
        };
        let back = match id {
            BACK => true,
            SIDEBAR_TOGGLE => false,
            _ => return,
        };
        if !has_area(shown) {
            return;
        }
        self.navigations.push(Navigation {
            back,
            at: shown,
            keys: keys.iter().rev().cloned().collect(),
            widget: node.widget.clone(),
            id: Some(id.to_owned()),
        });
    }

    /// A back control standing beside the sidebar toggle, in the same bar row: the window's
    /// leading corner offers two ways to navigate, where a screen that went somewhere deeper
    /// has one, its way back at the leading edge.
    fn double_navigation(&mut self) {
        let controls = std::mem::take(&mut self.navigations);
        for back in controls.iter().filter(|control| control.back) {
            let beside = controls.iter().any(|toggle| {
                !toggle.back
                    && back.at.y0 < toggle.at.y1 - EPSILON
                    && toggle.at.y0 < back.at.y1 - EPSILON
                    && back.at.x0 - toggle.at.x1 >= -EPSILON
                    && back.at.x0 - toggle.at.x1 <= toggle.at.height()
            });
            if !beside {
                continue;
            }
            self.out.push(Problem {
                kind: Kind::DoubleNavigation,
                keys: back.keys.clone(),
                widget: back.widget.clone(),
                id: back.id.clone(),
                detail: "the back control stands beside the sidebar toggle, where a screen \
                         that went somewhere deeper has one way back, at the leading edge"
                    .to_owned(),
            });
        }
    }

    /// A pane that only says nothing is chosen, standing beside the list the window chooses
    /// from: the pane takes the room the list would list in, and there is nothing in it to read.
    /// With nothing chosen the list lists for the whole window, and the pane appears when a row
    /// is chosen. A window whose list is its sidebar (a chat's channels over the conversation's
    /// place) keeps its pane: the rows of a sidebar's tree are no items to list beside it.
    fn placeholder_panes(&mut self, node: &InspectionNode, keys: &mut Vec<String>) {
        if node.widget == SHELL {
            let mut chosen = None;
            for (index, child) in node.children.iter().enumerate() {
                if child.widget != CHOICE_PANE
                    || words_of(child).is_empty()
                    || !pane_sized(rect(child.rect), self.viewport)
                {
                    continue;
                }
                if node.children.iter().enumerate().any(|(other, beside)| {
                    other != index && beside_rows(rect(child.rect), beside, self.viewport)
                }) {
                    chosen = Some(child);
                    break;
                }
            }
            if let Some(pane) = chosen {
                let pushed = pane.test_id.clone().map(|id| keys.push(id)).is_some();
                self.push(
                    Kind::PlaceholderPane,
                    pane,
                    keys,
                    format!(
                        "it says \"{}\" over {}, while the list of items beside it could list \
                         there until one is chosen",
                        words_of(pane),
                        size(rect(pane.rect))
                    ),
                );
                if pushed {
                    keys.pop();
                }
            }
        }
        for child in &node.children {
            let pushed = child.test_id.clone().map(|id| keys.push(id)).is_some();
            self.placeholder_panes(child, keys);
            if pushed {
                keys.pop();
            }
        }
    }

    fn small(&mut self, node: &InspectionNode, keys: &[String], hit: Rect, min: f64, what: &str) {
        if hit.width() < min - 0.5 || hit.height() < min - 0.5 {
            self.push(
                Kind::SmallHitTarget,
                node,
                keys,
                format!(
                    "{what} is {} where a target is at least {:.0}×{:.0}",
                    size(hit),
                    min,
                    min
                ),
            );
        }
    }

    /// Children of a box that lays them out side by side or in turn, covering each other. In a
    /// box that layers them on purpose (a badge over an image), only text over text: neither
    /// can be read. In a box that places its own parts over each other (a window shell: its
    /// columns, and the buttons of its header row in the same plane), only one control over
    /// another: a press lands on one of them, and their faces cover each other.
    fn overlaps(&mut self, node: &InspectionNode, keys: &[String], clip: Rect) {
        let layered = layers(&node.widget);
        let places = layered && node.widget != "Layers";
        let shown: Vec<(&InspectionNode, Rect)> = node
            .children
            .iter()
            .filter(|child| !floats(&child.widget))
            .map(|child| (child, rect(child.rect).intersect(clip)))
            .filter(|(_, r)| has_area(*r))
            .collect();
        for (i, (a, ra)) in shown.iter().enumerate() {
            for (b, rb) in shown.iter().skip(i.saturating_add(1)) {
                let both = ra.intersect(*rb);
                let covers = if stands_as_window(b) {
                    false
                } else if places {
                    is_control(a) && is_control(b)
                } else {
                    !layered || texts_meet(a, b, clip)
                };
                if has_area(both) && covers {
                    let mut keys = keys.to_vec();
                    keys.extend(b.test_id.clone());
                    self.push(
                        Kind::Overlap,
                        b,
                        &keys,
                        format!(
                            "{} {} covers {} of {} {}",
                            b.widget,
                            size(*rb),
                            size(both),
                            a.widget,
                            size(*ra)
                        ),
                    );
                }
            }
        }
    }

    fn push(&mut self, kind: Kind, node: &InspectionNode, keys: &[String], detail: String) {
        self.out.push(Problem {
            kind,
            keys: keys.iter().rev().cloned().collect(),
            widget: node.widget.clone(),
            id: node.test_id.clone(),
            detail,
        });
    }
}

/// The window's bar, as a finding names the widget it is about: the bar is the window's, not
/// the app's.
const BAR: &str = "WindowBar";

/// What is wrong with one window's bar as it lays itself out across the window: a control it
/// shows twice, and an action that asks for its words standing as its icon alone while the bar
/// has room for those words.
pub fn bar(layout: &ui::window_toolbar::BarLayout) -> Vec<Problem> {
    let mut out = Vec::new();
    for (index, control) in layout.controls.iter().enumerate() {
        let Some(later) = layout.controls.get(index.saturating_add(1)..) else {
            continue;
        };
        let Some(second) = later.iter().find(|second| same_control(control, second)) else {
            continue;
        };
        let what = |control: &ui::window_toolbar::BarControl| {
            if control.name.is_empty() {
                control.id.clone()
            } else {
                format!("\"{}\"", control.name)
            }
        };
        out.push(Problem {
            kind: Kind::DuplicateBarControl,
            keys: Vec::new(),
            widget: BAR.to_owned(),
            id: Some(control.id.clone()),
            detail: format!(
                "the bar shows {} and {} as one control: the {} control at x {:.0} and the {} \
                 control at x {:.0}, where one bar shows each control once and keeps one \"…\" \
                 for what does not fit in it",
                what(control),
                what(second),
                control.slot,
                control.rect[0],
                second.slot,
                second.rect[0],
            ),
        });
    }
    for control in &layout.controls {
        // Drawn as its icon alone, narrower than the words it has, while the room stood empty
        // beside it: a person cannot read what it does. The row's own "…" is a glyph by design.
        if control.slot == "overflow" || control.icon.is_none() || control.words <= 0.0 {
            continue;
        }
        let missing = control.words - control.rect[2];
        if missing <= EPSILON || layout.spare < missing {
            continue;
        }
        out.push(Problem {
            kind: Kind::IconOnlyBarAction,
            keys: Vec::new(),
            widget: BAR.to_owned(),
            id: Some(control.id.clone()),
            detail: format!(
                "the bar draws \"{}\" as its icon alone while {:.0}px of its location stand empty \
                 beside it, and the words need {:.0}px: a bar says what an action does where it \
                 has room for the words",
                control.name, layout.spare, control.words,
            ),
        });
    }
    out
}

/// Whether one bar shows `a` and `b` as the same control: the same action under two ids, or two
/// controls one glyph or one name apart, which a person cannot tell from each other. One bar
/// shows each control once, and keeps one "…" for what does not fit in it.
fn same_control(a: &ui::window_toolbar::BarControl, b: &ui::window_toolbar::BarControl) -> bool {
    if !a.id.is_empty() && a.id == b.id {
        return true;
    }
    if a.icon.is_some() || b.icon.is_some() {
        return a.icon == b.icon;
    }
    !a.name.is_empty() && a.name == b.name
}

/// Text whose widget needs more room than layout gave it: taller at its width than its box, or
/// with a word wider than its box. Each widget measures itself as layout measures it; a column
/// header's title, which the shell elides as it paints, is held to its row's rule by
/// [`header_row_problem`].
pub fn clipped_text(
    root: &mut dyn Widget,
    text: &mut ui::TextRenderer,
    viewport: (u32, u32),
) -> Vec<Problem> {
    let mut out = Vec::new();
    let clip = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
    measure_walk(root, &mut Vec::new(), clip, text, &mut out);
    out
}

/// How much room beside a title is room it should have taken: more than the widest gap two
/// neighbours in a header keep from each other.
const TITLE_ROOM: f64 = 24.0;

/// Whether a title that says `needs` wide is cut off in `has`, while `free` stands empty
/// beside it in its row.
fn title_cut_with_room(needs: f64, has: f64, free: f64) -> bool {
    needs > has + EPSILON && free > TITLE_ROOM
}

/// What is wrong with one header row, as the shell laid it out: a title cut off while an action
/// in the row kept a label it could have shown as its icon, a title cut off with room standing
/// empty beside it, or a row that kept its menu but not the app's primary action.
fn header_row_problem(row: &ui::widget::HeaderRowLayout) -> Option<String> {
    let needs = f64::from(row.title_needs);
    let has = f64::from(row.title_has);
    let free = f64::from(row.free_beside_title);
    if !row.primary_shown {
        return Some(format!(
            "the {} column's row keeps its menu but not its primary action",
            row.column
        ));
    }
    if needs <= has + EPSILON {
        return None;
    }
    if title_cut_with_room(needs, has, free) {
        return Some(format!(
            "the {} column's title says {needs:.0} wide, has {has:.0}, with {free:.0} free beside it",
            row.column
        ));
    }
    row.labelled_action.then(|| {
        format!(
            "the {} column's title says {needs:.0} wide, has {has:.0}, beside actions keeping \
             labels they could show as icons",
            row.column
        )
    })
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "window-space sizes narrowed for taffy; never near f32::MAX"
)]
fn measure_walk(
    widget: &mut dyn Widget,
    keys: &mut Vec<String>,
    clip: Rect,
    text: &mut ui::TextRenderer,
    out: &mut Vec<Problem>,
) {
    let pushed = widget
        .test_id()
        .map(|id| keys.push(id.to_owned()))
        .is_some();
    measure_one(widget, keys, clip, text, out);
    selection_lifts(widget, keys, out);
    let inner = widget
        .child_clip_rect()
        .map_or(clip, |child| clip.intersect(child));
    for child in widget.children_mut() {
        measure_walk(child, keys, inner, text, out);
    }
    if pushed {
        keys.pop();
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "window-space sizes narrowed for taffy; never near f32::MAX"
)]
fn measure_one(
    widget: &mut dyn Widget,
    keys: &[String],
    clip: Rect,
    text: &mut ui::TextRenderer,
    out: &mut Vec<Problem>,
) {
    use ui::taffy::{AvailableSpace, Size};
    let own = widget.get_rect();
    if has_area(own) && !has_area(own.intersect(clip)) {
        return;
    }
    if widget.name() == "Text" && has_area(own) {
        let width = own.width() as f32;
        let tall = widget.measure(
            Size {
                width: Some(width),
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::MaxContent,
            },
            text,
        );
        // The narrowest the text can be laid out at: a word longer than its line breaks
        // anywhere, so only text that cannot break (one line) needs more than a glyph.
        let narrowest = widget.measure(
            Size {
                width: None,
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(1.0),
                height: AvailableSpace::MaxContent,
            },
            text,
        );
        let shown = widget
            .accessibility()
            .and_then(|access| access.name)
            .unwrap_or_default();
        let detail = if f64::from(tall.height) > own.height() + EPSILON {
            Some(format!(
                "needs {:.0} high at its width, has {} ({shown:?})",
                tall.height,
                size(own)
            ))
        } else if f64::from(narrowest.width) > own.width() + EPSILON {
            Some(format!(
                "needs {:.0} wide at the narrowest, has {} ({shown:?})",
                narrowest.width,
                size(own)
            ))
        } else {
            None
        };
        if let Some(detail) = detail {
            out.push(Problem {
                kind: Kind::ClippedText,
                keys: keys.iter().rev().cloned().collect(),
                widget: widget.name().to_owned(),
                id: widget.test_id().map(str::to_owned),
                detail,
            });
        }
    }
}

/// A lift a list painted past the row it selects: it stands around what the row shows, grown by
/// the bleed a row grows its own face by ([`ui::widget::RowLift`]), so a lift reaching further
/// fills the gutters beside the row with the person's choice.
fn selection_lifts(widget: &dyn Widget, keys: &[String], out: &mut Vec<Problem>) {
    let Some(lifts) = ui::widget::capability::capability::<dyn ui::widget::PaintedRowLifts>(widget)
        .map(ui::widget::PaintedRowLifts::painted_row_lifts)
    else {
        return;
    };
    for lift in lifts {
        let Some(over) = reach_past(lift) else {
            continue;
        };
        out.push(Problem {
            kind: Kind::SelectionBleed,
            keys: keys.iter().rev().cloned().collect(),
            widget: widget.name().to_owned(),
            id: widget.test_id().map(str::to_owned),
            detail: format!(
                "selects {} of the {} its row shows, reaching {over:.0} past it",
                size(lift.rect),
                size(lift.content)
            ),
        });
    }
}

/// How far a lift reaches past the row it selects, more than the bleed a row's face grows by.
fn reach_past(lift: ui::widget::RowLift) -> Option<f64> {
    let room = Rect::new(
        lift.content.x0 - lift.bleed,
        lift.content.y0 - lift.bleed,
        lift.content.x1 + lift.bleed,
        lift.content.y1 + lift.bleed,
    );
    if lift.rect.width() <= 0.0 || lift.rect.height() <= 0.0 {
        return None;
    }
    [
        room.x0 - lift.rect.x0,
        lift.rect.x1 - room.x1,
        room.y0 - lift.rect.y0,
        lift.rect.y1 - room.y1,
    ]
    .into_iter()
    .fold(None, |most: Option<f64>, over| {
        (over > EPSILON)
            .then(|| most.map_or(over, |most| most.max(over)))
            .or(most)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::{AccessNode, Role};

    fn node(widget: &str, id: &str, rect: [f64; 4], role: Option<Role>) -> InspectionNode {
        InspectionNode {
            path: id.to_owned(),
            widget: widget.to_owned(),
            test_id: Some(id.to_owned()),
            focusable: role.is_some(),
            rect,
            child_clip: None,
            page_title: None,
            layout: None,
            semantics: role.map(AccessNode::new),
            children: Vec::new(),
        }
    }

    /// A title cut off with room beside it is the defect; one that fills its row, or has
    /// nothing to spare, is not.
    #[test]
    fn a_title_is_only_cut_off_unfairly_when_room_stands_beside_it() {
        assert!(title_cut_with_room(180.0, 99.0, 77.0));
        assert!(!title_cut_with_room(180.0, 180.0, 77.0));
        assert!(!title_cut_with_room(180.0, 99.0, 8.0));
        assert!(!title_cut_with_room(180.0, 99.0, 24.0));
    }

    fn header_row(
        needs: f32,
        has: f32,
        free: f32,
        labelled: bool,
        primary: bool,
    ) -> ui::widget::HeaderRowLayout {
        ui::widget::HeaderRowLayout {
            column: "content",
            title_needs: needs,
            title_has: has,
            free_beside_title: free,
            labelled_action: labelled,
            primary_shown: primary,
        }
    }

    /// A header row holds its title against its actions by one rule: a label the row could show
    /// as its icon gives way before the title is cut, room standing empty beside the title is
    /// the title's, and the app's primary action never leaves the row.
    #[test]
    fn a_header_row_gives_up_its_labels_before_its_title() {
        assert!(
            header_row_problem(&header_row(180.0, 180.0, 0.0, true, true)).is_none(),
            "a title that fits is cut off by nothing"
        );
        assert!(
            header_row_problem(&header_row(180.0, 180.0, 0.0, false, true)).is_none(),
            "labels stand while the row has room"
        );
        let labels = header_row_problem(&header_row(180.0, 99.0, 8.0, true, true))
            .expect("a title cut beside labels the row could show as icons");
        assert!(labels.contains("icons"), "{labels}");
        assert!(
            header_row_problem(&header_row(180.0, 99.0, 8.0, false, true)).is_none(),
            "a title cut beside actions with nothing left to give is the row's last resort"
        );
        let room = header_row_problem(&header_row(180.0, 99.0, 77.0, false, true))
            .expect("a title cut with room standing empty beside it");
        assert!(room.contains("free beside it"), "{room}");
        let dropped = header_row_problem(&header_row(180.0, 180.0, 0.0, false, false))
            .expect("a row that keeps its menu and not its primary action");
        assert!(dropped.contains("primary"), "{dropped}");
    }

    /// The shell reports how it held each header row, and the check holds it to the rule there.
    #[test]
    fn a_header_row_the_shell_reports_is_checked_where_it_stands() {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
        shell.layout = Some(ui::widget::LayoutInfo::Headers(ui::widget::HeaderRows {
            rows: [
                None,
                Some(header_row(120.0, 120.0, 0.0, false, false)),
                None,
                None,
            ],
        }));
        let found: Vec<Problem> = inspect(&shell, (400, 300), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::TitleClippedWithRoom)
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keys.first().map(String::as_str), Some("shell"));
        assert!(found[0].detail.contains("primary"), "{}", found[0].detail);
    }

    fn overlaps(children: Vec<InspectionNode>) -> Vec<Problem> {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
        shell.children = children;
        inspect(&shell, (400, 300), None, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::Overlap)
            .collect()
    }

    /// A window shell places its header row's buttons itself, over its columns: two of them
    /// covering each other (an action over the search button) is caught, though the shell
    /// layers its columns on purpose.
    #[test]
    fn controls_a_shell_places_over_each_other_are_caught() {
        let found = overlaps(vec![
            node(
                "Button",
                "search",
                [300.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
            node(
                "Button",
                "new",
                [316.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keys.first().map(String::as_str), Some("new"));
    }

    /// A column of a board as a height it did not get leaves it: the view scrolling its cards
    /// has no room, so the cards under its heading cannot be brought into view. Given the room
    /// of a pane, the same view holds them and nothing is lost.
    #[test]
    fn a_scroll_view_with_no_room_loses_the_cards_it_holds() {
        let lane = |height: f64| {
            let mut scrolling = stack_node(
                "lane",
                [0.0, 40.0, 240.0, height],
                (0..7)
                    .map(|index| {
                        stack_node(
                            "card",
                            [0.0, 40.0 + 76.0 * f64::from(index), 240.0, 68.0],
                            vec![],
                        )
                    })
                    .collect(),
            );
            scrolling.layout = Some(ui::widget::LayoutInfo::Scroll(ui::widget::ScrollLayout {
                direction: ui::widget::ScrollDirection::Vertical,
            }));
            stack_node("window", [0.0, 0.0, 400.0, 300.0], vec![scrolling])
        };
        let lost = |root: &InspectionNode| {
            inspect(root, (400, 300), None, 1.0)
                .into_iter()
                .filter(|problem| problem.kind == Kind::LostContent)
                .map(|problem| problem.keys)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            lost(&lane(0.0)),
            vec![vec!["lane".to_owned(), "window".to_owned()]],
            "a scroll view with no height holds seven cards in none of it"
        );
        assert!(
            lost(&lane(600.0)).is_empty(),
            "given the room of a pane, the same cards are there to scroll through"
        );
    }

    /// A row taking half the window is the page itself, and caught — unless its content is as
    /// tall as it is: a conversation's message fills its row whatever the row's height, and a
    /// row that tall is no page stretched around one line.
    #[test]
    fn a_tall_row_is_caught_only_when_its_line_floats_in_it() {
        let bare_rows = |content: InspectionNode| {
            let mut row = node("SwipeRow", "row", [0.0, 0.0, 400.0, 200.0], None);
            row.children.push(content);
            let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
            shell.children.push(row);
            inspect(&shell, (400, 300), None, 1.0)
                .into_iter()
                .filter(|problem| problem.kind == Kind::BareRow)
                .count()
        };
        assert_eq!(
            bare_rows(node("Row", "line", [0.0, 88.0, 400.0, 24.0], None)),
            1,
            "a line floating in the row's middle is the page drawn as one row"
        );
        assert_eq!(
            bare_rows(node("Row", "body", [0.0, 0.0, 400.0, 200.0], None)),
            0,
            "a row as tall as what it holds is as tall as its content"
        );
    }

    /// A scroll view's content stands in a box the view holds it in, which is the view's own
    /// content and no container of its own: what the view scrolls through may go on past the
    /// view. A box inside that content still holds what it holds, and is caught.
    #[test]
    fn a_scroll_views_content_box_passes_its_scrolling_on() {
        let scrolled = |content: InspectionNode| {
            let mut page = stack_node("page", [0.0, 0.0, 400.0, 300.0], vec![content]);
            page.layout = Some(ui::widget::LayoutInfo::Scroll(ui::widget::ScrollLayout {
                direction: ui::widget::ScrollDirection::Vertical,
            }));
            stack_node("window", [0.0, 0.0, 400.0, 300.0], vec![page])
        };
        let overflowing = |root: &InspectionNode| {
            inspect(root, (400, 300), None, 1.0)
                .into_iter()
                .filter(|problem| problem.kind == Kind::Overflow)
                .map(|problem| problem.keys)
                .collect::<Vec<_>>()
        };
        let page = scrolled(stack_node(
            "content",
            [0.0, 0.0, 380.0, 300.0],
            vec![stack_node(
                "long",
                [0.0, 0.0, 380.0, 900.0],
                vec![stack_node(
                    "card",
                    [0.0, 0.0, 380.0, 100.0],
                    vec![stack_node("over", [0.0, 0.0, 380.0, 240.0], vec![])],
                )],
            )],
        ));
        assert_eq!(
            overflowing(&page),
            vec![vec![
                "over".to_owned(),
                "card".to_owned(),
                "long".to_owned(),
                "content".to_owned(),
                "page".to_owned(),
                "window".to_owned()
            ]],
            "the content the view scrolls through may be taller than the view; the card in it may not"
        );
    }

    /// A column header that reads 0, and a place whose quiet count reads 0: a count says
    /// something wants attention, so zero is no badge and no room kept for one.
    #[test]
    fn a_count_of_zero_is_caught_where_it_is_drawn() {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
        let mut list = node(
            "NavList",
            "places",
            [0.0, 40.0, 280.0, 240.0],
            Some(Role::Tree),
        );
        list.semantics
            .as_mut()
            .expect("the list's semantics")
            .children = vec![
            place("Assigned to me", [0.0, 40.0, 280.0, 72.0], 0, false),
            place("Inbox", [0.0, 72.0, 280.0, 104.0], 3, false),
        ];
        shell.children = vec![
            list,
            counted(
                "NavShellTitle",
                "title",
                [300.0, 8.0, 380.0, 32.0],
                Role::Heading { level: 2 },
                "Done",
                "0",
            ),
        ];
        let zeros: Vec<Problem> = inspect(&shell, (400, 300), None, 1.0)
            .into_iter()
            .filter(|problem| problem.kind == Kind::ZeroCount)
            .collect();
        assert_eq!(zeros.len(), 2, "{zeros:?}");
    }

    /// A column header and the place beside it in the sidebar drawing the same number: the
    /// same count twice on one screen. A different number, or a place past the header, is not
    /// the same count again.
    #[test]
    fn a_header_count_the_place_beside_it_shows_is_caught_once() {
        let repeated = |header: u32, shown: u32, list_x0: f64| {
            let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
            let mut list = node(
                "NavList",
                "places",
                [list_x0, 40.0, list_x0 + 280.0, 240.0],
                Some(Role::Tree),
            );
            list.semantics
                .as_mut()
                .expect("the list's semantics")
                .children = vec![place(
                "Inbox",
                [list_x0, 40.0, list_x0 + 280.0, 72.0],
                shown,
                true,
            )];
            shell.children = vec![
                list,
                counted(
                    "NavShellTitle",
                    "title",
                    [600.0, 8.0, 680.0, 32.0],
                    Role::Heading { level: 2 },
                    "Inbox",
                    &header.to_string(),
                ),
            ];
            let root = stack_node("window", [0.0, 0.0, 900.0, 300.0], vec![shell]);
            inspect(&root, (900, 300), None, 1.0)
                .into_iter()
                .filter(|problem| problem.kind == Kind::RepeatedCount)
                .count()
        };
        assert_eq!(repeated(6, 6, 0.0), 1, "the sidebar beside it says 6");
        assert_eq!(repeated(6, 5, 0.0), 0, "the place says another number");
        assert_eq!(
            repeated(6, 6, 600.0),
            0,
            "a place past the header is no sidebar"
        );
    }

    fn counted(
        widget: &str,
        id: &str,
        rect: [f64; 4],
        role: Role,
        name: &str,
        value: &str,
    ) -> InspectionNode {
        let mut header = node(widget, id, rect, Some(role));
        let access = header.semantics.as_mut().expect("the header's semantics");
        access.name = Some(name.to_owned());
        access.value = Some(value.to_owned());
        header
    }

    /// A bar naming the page while the page shows those words as its own title: the same title
    /// twice on one screen. A bar whose title is already covered, or a page whose heading says
    /// other words, is no repeat.
    #[test]
    fn a_bar_naming_a_page_beside_the_pages_own_heading_is_caught_once() {
        let repeated = |bar_title: Option<&str>, page: Option<&str>| {
            let mut shell = node("NavShell", "shell", [0.0, 0.0, 390.0, 844.0], None);
            let mut bar = node(
                "NavShellTitle",
                "bar",
                [68.0, 18.0, 314.0, 39.0],
                Some(Role::Heading { level: 2 }),
            );
            if let Some(title) = bar_title {
                bar.semantics.as_mut().expect("the bar's semantics").name = Some(title.to_owned());
            } else {
                bar.semantics = None;
            }
            let mut heading = node("TextInput", "title", [80.0, 80.0, 294.0, 161.0], None);
            heading.page_title = page.map(str::to_owned);
            shell.children = vec![bar, heading];
            inspect(&shell, (390, 844), None, 1.0)
                .into_iter()
                .filter(|problem| problem.kind == Kind::RepeatedPageTitle)
                .count()
        };
        assert_eq!(
            repeated(
                Some("Call the landlord about the boiler"),
                Some("Call the landlord about the boiler")
            ),
            1,
            "the page's heading says what the bar says"
        );
        assert_eq!(
            repeated(Some("Call the landlord about the boiler"), None),
            0,
            "the page shows no title of its own, so the bar names it"
        );
        assert_eq!(
            repeated(None, Some("Call the landlord about the boiler")),
            0,
            "the bar left the title to the page"
        );
        assert_eq!(
            repeated(
                Some("Call the landlord about the boiler"),
                Some("Call the plumber")
            ),
            0,
            "the page shows other words"
        );
    }

    /// A bar with one way back has that way at its leading edge. A back control standing beside
    /// the sidebar toggle is two ways from one corner: caught.
    #[test]
    fn a_back_control_beside_the_sidebar_toggle_is_caught() {
        let beside = |toggle: Option<[f64; 4]>, back: Option<[f64; 4]>| {
            let mut shell = node("NavShell", "shell", [0.0, 0.0, 390.0, 844.0], None);
            let control = |id: &str, rect: [f64; 4]| node("Button", id, rect, Some(Role::Button));
            shell.children = [
                toggle.map(|rect| control(SIDEBAR_TOGGLE, rect)),
                back.map(|rect| control(BACK, rect)),
            ]
            .into_iter()
            .flatten()
            .collect();
            inspect(&shell, (390, 844), None, 1.0)
                .into_iter()
                .filter(|problem| problem.kind == Kind::DoubleNavigation)
                .count()
        };
        // A toggle named for the window's corner, and back after it.
        let toggle = Some([8.0, 18.0, 44.0, 44.0]);
        let beside_toggle = Some([60.0, 18.0, 44.0, 44.0]);
        assert_eq!(
            beside(toggle, beside_toggle),
            1,
            "the phone's pushed screen shows the toggle and back side by side"
        );
        assert_eq!(
            beside(toggle, Some([8.0, 18.0, 44.0, 44.0])),
            0,
            "back at the leading edge is the one way back"
        );
        assert_eq!(
            beside(None, beside_toggle),
            0,
            "a screen without a sidebar toggle has only its way back"
        );
        assert_eq!(
            beside(toggle, Some([300.0, 18.0, 44.0, 44.0])),
            0,
            "a docked sidebar's toggle stands in the window's corner, back in its column"
        );
    }

    fn place(name: &str, rect: [f64; 4], count: u32, selected: bool) -> AccessNode {
        let mut place = AccessNode::new(Role::TreeItem);
        place.name = Some(name.to_owned());
        place.value = Some(count.to_string());
        place.state.selected = selected;
        place.bounds =
            Some(Rect::new(rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]).into());
        place
    }

    fn inspected(root: &InspectionNode) -> Vec<Problem> {
        inspect(root, (800, 600), None, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::Overlap)
            .collect()
    }

    fn lone_doors(root: &InspectionNode) -> Vec<Problem> {
        inspect(root, (1280, 800), None, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::LoneDoor)
            .collect()
    }

    /// A page's "…" is the item's actions at the trailing edge of the title's line. One
    /// standing in a column takes a line of the document for its glyph alone, under the title:
    /// caught. Beside the title, and in the window bar, a menu is where it belongs.
    #[test]
    fn a_pages_dots_are_caught_only_off_the_titles_line() {
        let page = |content: InspectionNode| {
            stack_node(
                "window",
                [0.0, 0.0, 1280.0, 800.0],
                vec![stack_node(
                    "page",
                    [0.0, 56.0, 1280.0, 200.0],
                    vec![content],
                )],
            )
        };
        let door = || {
            node(
                "MenuButton",
                "detail~more",
                [888.0, 104.0, 32.0, 32.0],
                Some(Role::Button),
            )
        };
        let alone = page(stack_node(
            "content",
            [360.0, 104.0, 560.0, 85.0],
            vec![door()],
        ));
        let found = lone_doors(&alone);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].keys.first().map(String::as_str),
            Some("detail~more"),
            "at the page's door"
        );
        let line = page(holding(
            node("Row", "title-line", [360.0, 104.0, 560.0, 49.0], None),
            vec![
                node("TextInput", "detail", [360.0, 104.0, 520.0, 49.0], None),
                door(),
            ],
        ));
        assert!(
            lone_doors(&line).is_empty(),
            "beside its title, the door is where it belongs"
        );
        let bar = page(node(
            "MenuButton",
            "nav-shell-more-3",
            [1220.0, 12.0, 32.0, 32.0],
            Some(Role::Button),
        ));
        assert!(
            lone_doors(&bar).is_empty(),
            "the window bar's own menu is no page's door"
        );
    }

    fn empty_headings(root: &InspectionNode) -> Vec<Problem> {
        inspect(root, (1280, 800), None, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::EmptyHeading)
            .collect()
    }

    /// A heading nobody named takes its band's height over a lane's cards all the same: the
    /// strip says nothing, and the cards start below it. Named, it is the lane's words; a lane
    /// no heading heads starts its cards where a list starts its first row.
    #[test]
    fn a_heading_with_nothing_in_it_is_caught() {
        let heading = |name: &str| {
            let words = !name.is_empty();
            let mut header = node(
                HEADING,
                "heading",
                [304.0, 80.0, 320.0, 29.0],
                Some(Role::Heading { level: 2 }),
            );
            header.semantics = Some(AccessNode {
                name: words.then(|| name.to_owned()),
                ..AccessNode::new(Role::Heading { level: 2 })
            });
            let text = if words {
                [304.0, 84.0, 69.0, 21.0]
            } else {
                [304.0, 94.0, 0.0, 0.0]
            };
            header.children = vec![node(
                "Text",
                "title",
                text,
                Some(Role::Heading { level: 2 }),
            )];
            header
        };
        let lane = |header: Option<InspectionNode>| {
            let top = if header.is_some() { 117.0 } else { 80.0 };
            let cards = stack_node(
                "cards",
                [304.0, top, 320.0, 200.0],
                vec![node("ListRow", "card", [304.0, top, 320.0, 49.0], None)],
            );
            let column = holding(
                node("Column", "column", [304.0, 80.0, 320.0, 300.0], None),
                header.into_iter().chain([cards]).collect(),
            );
            holding(
                node("Container", "lane", [304.0, 80.0, 320.0, 300.0], None),
                vec![column],
            )
        };
        let board = |lane: InspectionNode| {
            holding(
                node("Row", "board", [280.0, 80.0, 1000.0, 600.0], None),
                vec![lane],
            )
        };
        let found = empty_headings(&board(lane(Some(heading("")))));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keys.first().map(String::as_str), Some("heading"));
        assert!(
            empty_headings(&board(lane(Some(heading("Doing"))))).is_empty(),
            "a named heading is the lane's words"
        );
        assert!(
            empty_headings(&board(lane(None))).is_empty(),
            "a lane no heading heads starts its cards at its top"
        );
    }

    fn empty_surfaces(root: &InspectionNode) -> Vec<Problem> {
        inspect(root, (800, 600), None, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::EmptySurface)
            .collect()
    }

    /// A card standing empty in a column paints a filled block with nothing in it, and what it
    /// promised is not there: a surface is drawn for what it holds, so one holding nothing — or
    /// only a placeholder that never filled — is no surface at all. What holds words, a control
    /// or anything laid out of its own is a surface doing its work.
    #[test]
    fn a_surface_with_nothing_in_it_is_caught() {
        let column = |surface: Option<InspectionNode>, under: Option<InspectionNode>| {
            let mut children: Vec<InspectionNode> = surface.into_iter().collect();
            children.extend(under);
            holding(
                node("Column", "column", [24.0, 56.0, 342.0, 600.0], None),
                children,
            )
        };
        let card = |children: Vec<InspectionNode>| {
            holding(
                node("Card", "card", [24.0, 56.0, 342.0, 157.0], None),
                children,
            )
        };
        let found = empty_surfaces(&column(Some(card(Vec::new())), None));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keys.first().map(String::as_str), Some("card"));
        assert!(
            found[0].detail.contains("342×157"),
            "it names the block it paints: {}",
            found[0].detail
        );
        let placeholder = card(vec![node("Card", "body", [24.0, 56.0, 0.0, 0.0], None)]);
        assert_eq!(
            empty_surfaces(&column(Some(placeholder), None)).len(),
            1,
            "a placeholder that never filled leaves the block empty"
        );
        let written = card(vec![node(
            "Text",
            "body",
            [44.0, 76.0, 302.0, 21.0],
            Some(Role::StaticText),
        )]);
        assert!(
            empty_surfaces(&column(Some(written), None)).is_empty(),
            "a card saying something is what the block is for"
        );
        let pictured = card(vec![node(
            "Picture",
            "cover",
            [44.0, 76.0, 302.0, 120.0],
            None,
        )]);
        assert!(
            empty_surfaces(&column(Some(pictured), None)).is_empty(),
            "a card showing a picture holds something"
        );
        let flat = holding(
            node("Card", "flat", [24.0, 56.0, 342.0, 0.0], None),
            Vec::new(),
        );
        assert!(
            empty_surfaces(&column(Some(flat), None)).is_empty(),
            "a block with no height paints nothing"
        );
        assert!(
            empty_surfaces(&column(
                None,
                Some(node("Stack", "rows", [24.0, 56.0, 342.0, 200.0], None))
            ))
            .is_empty(),
            "a column is no surface of its own"
        );
    }

    fn named(widget: &str, id: &str, rect: [f64; 4], said: &str) -> InspectionNode {
        let mut boxed = node(widget, id, rect, Some(Role::Group));
        if let Some(access) = boxed.semantics.as_mut() {
            access.name = Some(said.to_owned());
        }
        boxed
    }

    fn placeholder_panes(root: &InspectionNode) -> Vec<Problem> {
        inspect(root, (1280, 800), None, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::PlaceholderPane)
            .collect()
    }

    /// A pane that only says nothing is chosen, standing beside the list the window chooses
    /// from, takes the room the rows would list in: with nothing chosen the list lists for the
    /// whole window, and the pane appears when a row is chosen. A window that lists no items
    /// beside it keeps its pane, and so does a phone, where the pane stands over the list it
    /// pushed aside rather than beside it.
    #[test]
    fn a_pane_saying_nothing_is_chosen_beside_the_list_is_caught() {
        let window = [0.0, 0.0, 1280.0, 800.0];
        let list = |rect: [f64; 4]| {
            holding(
                node("DescribedList", "notes", rect, None),
                vec![node(
                    "SwipeRow",
                    "note",
                    [rect[0] + 24.0, rect[1], 296.0, 52.0],
                    None,
                )],
            )
        };
        let chosen = |rect: [f64; 4]| named("EmptyState", "chosen", rect, "No note selected");
        let shell = |children: Vec<InspectionNode>| {
            holding(node("NavShell", "shell", window, None), children)
        };
        let found = placeholder_panes(&shell(vec![
            list([280.0, 56.0, 344.0, 744.0]),
            chosen([624.0, 56.0, 656.0, 744.0]),
        ]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keys.first().map(String::as_str), Some("chosen"));
        assert!(
            found[0].detail.contains("\"No note selected\""),
            "it names what the pane says: {}",
            found[0].detail
        );
        assert!(
            placeholder_panes(&shell(vec![chosen([624.0, 56.0, 656.0, 744.0])])).is_empty(),
            "a window listing nothing keeps its pane"
        );
        let mut places = node("NavList", "places", [280.0, 56.0, 344.0, 744.0], None);
        places.children = vec![node(
            "NavItem",
            "place",
            [280.0, 56.0, 344.0, 32.0],
            Some(Role::TreeItem),
        )];
        assert!(
            placeholder_panes(&shell(vec![places, chosen([624.0, 56.0, 656.0, 744.0])])).is_empty(),
            "the places of a sidebar are no list of items to choose from"
        );
        assert!(
            placeholder_panes(&shell(vec![
                list([-1000.0, 56.0, 344.0, 744.0]),
                chosen([0.0, 56.0, 1280.0, 744.0]),
            ]))
            .is_empty(),
            "a phone's pane stands over the list it pushed aside, not beside it"
        );
        assert!(
            placeholder_panes(&shell(vec![
                list([280.0, 56.0, 344.0, 744.0]),
                chosen([624.0, 300.0, 100.0, 60.0]),
            ]))
            .is_empty(),
            "a notice inside a column is no pane of its own"
        );
    }

    fn bare_rows(root: &InspectionNode) -> Vec<Problem> {
        inspect(root, (800, 600), None, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::BareRow)
            .collect()
    }

    /// A page whose content is one row: the row is stretched over the column and its line
    /// floats at the column's middle, where a page around it would hold it at the top. A list's
    /// rows are their line's height, and a page that holds a row is no finding.
    #[test]
    fn a_page_drawn_as_one_row_is_caught() {
        let shell = |content: Vec<InspectionNode>| {
            let mut shell = node("NavShell", "shell", [0.0, 0.0, 800.0, 600.0], None);
            shell.children = content;
            shell
        };
        let bare = shell(vec![node(
            "ListRow",
            "detail-row",
            [416.0, 56.0, 384.0, 544.0],
            Some(Role::ListItem),
        )]);
        let found = bare_rows(&bare);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].keys.first().map(String::as_str),
            Some("detail-row"),
            "at the row a view drew"
        );
        let listed = shell(vec![holding(
            node("DescribedList", "rows", [76.0, 56.0, 340.0, 540.0], None),
            vec![
                node("ListRow", "a", [76.0, 56.0, 340.0, 36.0], None),
                node("ListRow", "b", [76.0, 92.0, 340.0, 36.0], None),
            ],
        )]);
        assert!(bare_rows(&listed).is_empty(), "a list's rows are lines");
        let page = shell(vec![holding(
            node("Scroll", "scroll", [416.0, 56.0, 384.0, 544.0], None),
            vec![holding(
                node("page", "page", [416.0, 56.0, 384.0, 544.0], None),
                vec![holding(
                    node("stack", "stack", [440.0, 80.0, 336.0, 120.0], None),
                    vec![node(
                        "ListRow",
                        "summary",
                        [440.0, 80.0, 336.0, 60.0],
                        Some(Role::ListItem),
                    )],
                )],
            )],
        )]);
        assert!(
            bare_rows(&page).is_empty(),
            "a page holding a row is no finding"
        );
    }

    fn holding(mut outer: InspectionNode, children: Vec<InspectionNode>) -> InspectionNode {
        outer.children = children;
        outer
    }

    /// Layers that each hold text somewhere cover nothing until the text itself meets: a bar
    /// across the top and a dock on the foot of a desk drawn over the whole window are two
    /// layers whose words stand apart; a caption drawn on another caption is caught.
    #[test]
    fn layers_cover_each_other_only_where_their_text_meets() {
        let layer = |id: &str, text: [f64; 4]| {
            holding(
                node("Stack", id, [0.0, 0.0, 800.0, 600.0], None),
                vec![node("Text", &format!("{id}-text"), text, None)],
            )
        };
        let apart = holding(
            node("Layers", "desk", [0.0, 0.0, 800.0, 600.0], None),
            vec![
                layer("bar", [8.0, 8.0, 120.0, 20.0]),
                layer("dock", [300.0, 560.0, 40.0, 20.0]),
            ],
        );
        assert!(inspected(&apart).is_empty());
        let meeting = holding(
            node("Layers", "desk", [0.0, 0.0, 800.0, 600.0], None),
            vec![
                layer("bar", [8.0, 8.0, 120.0, 20.0]),
                layer("over", [60.0, 12.0, 120.0, 20.0]),
            ],
        );
        assert_eq!(inspected(&meeting).len(), 1);
        // Text a scroller has moved out of its view is not read, so it covers nothing: a
        // conversation scrolled to its end, its first messages above its top edge.
        let mut scroller = holding(
            node("Scroll", "scroller", [0.0, 100.0, 800.0, 400.0], None),
            vec![node("Text", "scrolled-out", [8.0, 8.0, 120.0, 20.0], None)],
        );
        scroller.child_clip = Some([0.0, 100.0, 800.0, 400.0]);
        let desk_over_scroller = holding(
            node("Layers", "desk", [0.0, 0.0, 800.0, 600.0], None),
            vec![
                layer("bar", [8.0, 8.0, 120.0, 20.0]),
                holding(
                    node("Stack", "window", [0.0, 0.0, 800.0, 600.0], None),
                    vec![scroller],
                ),
            ],
        );
        assert!(inspected(&desk_over_scroller).is_empty());
    }

    /// Windows on a desktop stand over each other by design: the one drawn later is opaque
    /// and covers the one under it, and that is no finding; two plain boxes side by side in a
    /// free placement covering each other still are.
    #[test]
    fn a_window_over_another_window_is_no_finding() {
        let window = |id: &str, at: [f64; 4]| {
            holding(
                node("TilolangPane", id, at, None),
                vec![holding(
                    node("WindowFrame", &format!("{id}-frame"), at, None),
                    vec![node(
                        "Text",
                        &format!("{id}-title"),
                        [at[0] + 16.0, at[1] + 12.0, 100.0, 20.0],
                        None,
                    )],
                )],
            )
        };
        let desk = holding(
            node("Manual", "desk", [0.0, 0.0, 800.0, 600.0], None),
            vec![
                window("behind", [40.0, 40.0, 400.0, 300.0]),
                window("front", [200.0, 100.0, 400.0, 300.0]),
            ],
        );
        assert!(inspected(&desk).is_empty());
        let boxes = holding(
            node("Manual", "desk", [0.0, 0.0, 800.0, 600.0], None),
            vec![
                node("Stack", "a", [40.0, 40.0, 400.0, 300.0], None),
                node("Stack", "b", [200.0, 100.0, 400.0, 300.0], None),
            ],
        );
        assert_eq!(inspected(&boxes).len(), 1);
    }

    /// On touch a control smaller than a finger's target is caught, at its own box grown by
    /// what it reports; with a mouse, whose hit areas grow about the face, nothing is.
    #[test]
    fn a_control_smaller_than_a_finger_is_caught_on_touch() {
        let mut root = node("Column", "root", [0.0, 0.0, 400.0, 300.0], None);
        root.children = vec![
            node(
                "Button",
                "small",
                [16.0, 16.0, 32.0, 32.0],
                Some(Role::Button),
            ),
            node(
                "Button",
                "large",
                [16.0, 64.0, 48.0, 48.0],
                Some(Role::Button),
            ),
        ];
        let small = |min| {
            inspect(&root, (400, 300), min, 1.0)
                .into_iter()
                .filter(|p| p.kind == Kind::SmallHitTarget)
                .map(|p| p.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(small(Some(48.0)), [Some("small".to_owned())]);
        assert!(small(None).is_empty());
    }

    /// A long word in a column narrower than it: text that wraps breaks it anywhere and shows
    /// all of it; text that keeps its words whole (source code's soft wrap) is clipped, and
    /// caught.
    #[test]
    fn text_that_cannot_break_a_word_wider_than_its_column_is_caught() {
        use ui::widget::arrange::{Length, Slot, Stack};
        let clipped = |mode: ui::text_render::TextWrapMode| {
            let text =
                ui::widget::Text::new("Donaudampfschifffahrtsgesellschaft").with_wrap_mode(mode);
            let mut column = Slot::new(Box::new(Stack::stack().push(Box::new(text))));
            column.fit.width.max = Some(Length::Rem(6.0));
            let mut h = ui::headless::FrameHarness::for_root(
                ui::State::new(),
                Stack::stack().push_slot(column),
                (320, 200),
                ui::Theme::light(),
            );
            let mut renderer = ui::TextRenderer::new();
            clipped_text(h.root_widget_mut(), &mut renderer, (320, 200)).len()
        };
        assert_eq!(clipped(ui::text_render::TextWrapMode::Wrap), 0);
        assert_eq!(clipped(ui::text_render::TextWrapMode::Indented), 1);
    }

    /// A place on a canvas past its viewport is reached by panning, so it is no finding; the
    /// same place in a region of a plain column reaches past it, and is caught.
    #[test]
    fn a_place_past_a_canvas_viewport_pans_and_past_a_column_overflows() {
        use ui::widget::{LayoutInfo, ManualLayout, ScrollDirection, ScrollLayout};
        let found = |outer: Option<LayoutInfo>| {
            let mut region = node("Manual", "region", [0.0, 0.0, 320.0, 300.0], None);
            region.layout = Some(LayoutInfo::Manual(ManualLayout {
                allocation: [320.0, 300.0],
                requested_bounds: [0.0, 0.0, 320.0, 300.0],
                content_bounds: [0.0, 0.0, 560.0, 300.0],
                overflow: false,
                first_conflict: None,
            }));
            region.children = vec![holding(
                node("Card", "place", [200.0, 40.0, 240.0, 90.0], None),
                vec![node(
                    "Text",
                    "place-name",
                    [204.0, 44.0, 236.0, 86.0],
                    Some(Role::StaticText),
                )],
            )];
            let mut view = node("View", "view", [0.0, 0.0, 320.0, 300.0], None);
            view.child_clip = Some([0.0, 0.0, 320.0, 300.0]);
            view.layout = outer;
            view.children = vec![region];
            // The window is wider than the view, so the place stays within the window and only
            // the view's own box is passed.
            inspect(&view, (600, 300), None, 1.0)
                .into_iter()
                .map(|p| p.kind)
                .collect::<Vec<_>>()
        };
        let canvas = Some(LayoutInfo::Scroll(ScrollLayout {
            direction: ScrollDirection::Both,
        }));
        assert_eq!(found(canvas), []);
        assert!(found(None).contains(&Kind::Overflow));
    }

    /// A header action the shell holds in a frame of its own is the control it frames.
    #[test]
    fn a_framed_action_over_a_button_is_caught() {
        let mut frame = node("Row", "actions", [316.0, 12.0, 32.0, 32.0], None);
        frame.children = vec![node(
            "Button",
            "new",
            [316.0, 12.0, 32.0, 32.0],
            Some(Role::Button),
        )];
        let found = overlaps(vec![
            node(
                "Button",
                "search",
                [300.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
            frame,
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    /// What a shell layers on purpose is no finding: a column under its header's buttons, and
    /// buttons side by side.
    #[test]
    fn a_shells_columns_and_buttons_side_by_side_are_not() {
        let mut column = node("Column", "content", [0.0, 0.0, 400.0, 300.0], None);
        column.children = vec![
            node("Text", "a", [16.0, 60.0, 100.0, 20.0], None),
            node("Text", "b", [16.0, 90.0, 100.0, 20.0], None),
        ];
        let found = overlaps(vec![
            column,
            node(
                "Button",
                "search",
                [300.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
            node(
                "Button",
                "new",
                [340.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    fn stack_node(id: &str, rect: [f64; 4], children: Vec<InspectionNode>) -> InspectionNode {
        InspectionNode {
            path: id.to_owned(),
            widget: "Stack".to_owned(),
            test_id: Some(id.to_owned()),
            focusable: false,
            rect,
            child_clip: None,
            page_title: None,
            layout: None,
            semantics: None,
            children,
        }
    }

    fn off_pixel(root: &InspectionNode, scale: f64) -> Vec<Vec<String>> {
        inspect(root, (400, 300), None, scale)
            .into_iter()
            .filter(|problem| problem.kind == Kind::OffPixel)
            .map(|problem| problem.keys)
            .collect()
    }

    #[test]
    fn an_edge_between_device_pixels_is_found_at_the_outermost_box_it_moves() {
        // 20.5 is a device pixel at 2x but not at 1x; 20 + 2/3 is one at 1.5x only.
        let third = 20.0 + 2.0 / 3.0;
        let window = |inner: [f64; 4]| {
            stack_node(
                "window",
                [0.0, 0.0, 400.0, 300.0],
                vec![stack_node(
                    "column",
                    inner,
                    vec![stack_node(
                        "label",
                        [inner[0], inner[1], 50.0, 10.0],
                        vec![],
                    )],
                )],
            )
        };
        let half = window([20.5, 10.0, 100.0, 40.0]);
        assert_eq!(
            off_pixel(&half, 1.0),
            vec![vec!["column".to_owned(), "window".to_owned()]],
            "found at the column, not again at the label it moves"
        );
        assert!(off_pixel(&half, 2.0).is_empty());
        assert_eq!(off_pixel(&half, 1.5).len(), 1);

        let thirds = window([third, 10.0, 100.0, 40.0]);
        assert!(off_pixel(&thirds, 1.5).is_empty());
        assert_eq!(off_pixel(&thirds, 2.0).len(), 1);
        assert_eq!(off_pixel(&thirds, 1.0).len(), 1);

        assert_eq!(
            off_pixel(&window([21.0, 10.0, 100.0, 40.0]), 1.5).len(),
            1,
            "a whole logical pixel is not a device pixel at 1.5x"
        );
        assert!(off_pixel(&window([20.0, 10.0, 100.0, 40.0]), 1.5).is_empty());
    }

    /// A bar's control: `id` in `slot`, named `name`, showing `icon`, asking for `words` wide of
    /// them where it is a menu button (`menu`).
    fn control(
        slot: &'static str,
        id: &str,
        name: &str,
        icon: Option<&str>,
        words: f64,
        menu: bool,
    ) -> ui::window_toolbar::BarControl {
        let control =
            ui::window_toolbar::BarControl::new(slot, id, Rect::new(0.0, 0.0, 32.0, 28.0))
                .named(name)
                .icon(icon.map(str::to_owned))
                .words(words);
        if menu { control.menu() } else { control }
    }

    fn bar(
        controls: Vec<ui::window_toolbar::BarControl>,
        spare: f64,
    ) -> ui::window_toolbar::BarLayout {
        ui::window_toolbar::BarLayout { spare, controls }
    }

    fn kinds(layout: &ui::window_toolbar::BarLayout) -> Vec<Kind> {
        super::bar(layout)
            .into_iter()
            .map(|problem| problem.kind)
            .collect()
    }

    /// One bar shows each control once: the same action under two ids, or two controls one glyph
    /// apart, is one control too many.
    #[test]
    fn a_bar_shows_each_control_once() {
        let twice = bar(
            vec![
                control(
                    "leading",
                    "tasks~check",
                    "Complete",
                    Some("circle-check"),
                    0.0,
                    false,
                ),
                control(
                    "action",
                    "page~check",
                    "Complete",
                    Some("circle-check"),
                    0.0,
                    false,
                ),
                control(
                    "overflow",
                    "window-toolbar-more",
                    "More",
                    Some("more-horizontal"),
                    0.0,
                    true,
                ),
                control("close", "window-close", "Close", Some("x"), 0.0, false),
            ],
            0.0,
        );
        assert_eq!(
            kinds(&twice),
            vec![Kind::DuplicateBarControl],
            "the two check circles are one control to a person"
        );
    }

    /// The app's own door beside the bar's own "…": one bar has one.
    #[test]
    fn an_apps_own_door_beside_the_bars_is_one_door_too() {
        let doors = bar(
            vec![
                control(
                    "action",
                    "page~more",
                    "More actions",
                    Some("more-horizontal"),
                    0.0,
                    true,
                ),
                control(
                    "overflow",
                    "window-toolbar-more",
                    "More",
                    Some("more-horizontal"),
                    0.0,
                    true,
                ),
            ],
            0.0,
        );
        assert_eq!(kinds(&doors), vec![Kind::DuplicateBarControl]);
        // Every control of a window's own bar stands once: its navigation, its "…", its
        // controls.
        let window = bar(
            vec![
                control(
                    "sidebar",
                    "nav-shell-sidebar",
                    "Show sidebar",
                    None,
                    0.0,
                    false,
                ),
                control("back", "nav-shell-back", "Back", None, 0.0, false),
                control(
                    "action",
                    "page~clear",
                    "Clear done",
                    Some("circle-check"),
                    0.0,
                    false,
                ),
                control(
                    "overflow",
                    "window-toolbar-more",
                    "More",
                    Some("more-horizontal"),
                    0.0,
                    true,
                ),
                control("history", "window-history", "History", None, 0.0, false),
                control(
                    "minimize",
                    "window-minimize",
                    "Minimize",
                    Some("minus"),
                    0.0,
                    false,
                ),
                control(
                    "maximize",
                    "window-maximize",
                    "Maximize",
                    Some("maximize-2"),
                    0.0,
                    false,
                ),
                control("close", "window-close", "Close", Some("x"), 0.0, false),
            ],
            400.0,
        );
        assert!(kinds(&window).is_empty(), "{:?}", kinds(&window));
    }

    /// An action that asks for its words stands with them where the bar has room, and as its
    /// icon only where it has not.
    #[test]
    fn a_bar_says_what_an_action_does_where_it_has_room_for_the_words() {
        let icon = |spare: f64| {
            bar(
                vec![control(
                    "action",
                    "page~clear",
                    "Clear done",
                    Some("circle-check"),
                    64.0,
                    false,
                )],
                spare,
            )
        };
        assert_eq!(kinds(&icon(400.0)), vec![Kind::IconOnlyBarAction]);
        assert!(kinds(&icon(20.0)).is_empty(), "a narrow bar keeps the icon");
        // A menu button says what it opens like any other action where the row has room.
        let menu = bar(
            vec![control(
                "action",
                "page~sort",
                "Sort",
                Some("arrow-up-down"),
                48.0,
                true,
            )],
            400.0,
        );
        assert_eq!(kinds(&menu), vec![Kind::IconOnlyBarAction]);
        // A labelled action stands as wide as its words: the row gave it the room.
        let labelled = bar(
            vec![
                ui::window_toolbar::BarControl::new(
                    "action",
                    "page~clear",
                    Rect::new(0.0, 0.0, 64.0, 28.0),
                )
                .named("Clear done")
                .icon(Some("circle-check".to_owned()))
                .labelled()
                .words(64.0),
            ],
            400.0,
        );
        assert!(kinds(&labelled).is_empty());
        // An action that asks for no words, like a bar's own plus, is its icon alone.
        let quiet = bar(
            vec![control(
                "action",
                "page~add",
                "New task",
                Some("plus"),
                0.0,
                false,
            )],
            400.0,
        );
        assert!(kinds(&quiet).is_empty());
    }

    /// A choice a list painted wide of what its row shows fills the gutter beside it: the lift
    /// the list recorded is read against the row's own box, grown by the row's bleed and no
    /// further, so the strip the person sees stands where the row does.
    #[test]
    fn a_lift_reaching_past_what_its_row_shows_is_a_selection_bleed() {
        let lift = |whole: [f64; 4], content: [f64; 4]| ui::widget::RowLift {
            rect: rect(whole),
            content: rect(content),
            bleed: 8.0,
        };
        let inset = lift([624.0, 100.0, 1280.0, 148.0], [672.0, 100.0, 1232.0, 148.0]);
        assert_eq!(reach_past(inset), Some(40.0), "the gutters either side");
        let around = lift([664.0, 100.0, 1240.0, 148.0], [672.0, 100.0, 1232.0, 148.0]);
        assert_eq!(reach_past(around), None, "a lift grown by the row's bleed");
        let whole = lift([0.0, 0.0, 300.0, 40.0], [0.0, 0.0, 300.0, 40.0]);
        assert_eq!(reach_past(whole), None, "a row showing everything it has");
        let empty = lift([0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(reach_past(empty), None, "no lift at all");
    }
}
