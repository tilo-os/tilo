//! What is wrong with one laid-out window: read off the toolkit's inspection of the frame (its
//! widgets' boxes, clips and semantics) and, for text, the widgets' own measurements.
//!
//! The bar's own layout is [`bar`]'s, and what text measures against its box is [`text`]'s.

mod bar;
mod kind;
mod text;

pub use bar::{bar, stacked_header_rows};
pub use kind::Kind;
pub use text::clipped_text;
use text::header_row_problem;
#[cfg(test)]
use text::{reach_past, title_cut_with_room};

#[cfg(test)]
mod tests;

use ui::headless::InspectionNode;
use ui::kurbo::Rect;

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

/// A page's widget: the readable column a screen's content stands on.
const PAGE: &str = "Page";

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

/// The first page a node holds, at any depth through what it lays out: a page given a column of
/// its own inside another page.
fn held_page(node: &InspectionNode) -> Option<&InspectionNode> {
    node.children
        .iter()
        .filter(|child| !floats(&child.widget) && !floats_in_window(&child.widget))
        .find_map(|child| {
            if child.widget == PAGE {
                return Some(child);
            }
            held_page(child)
        })
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
    /// The page the node stands in: the box that scrolls a window's content under its heading.
    page: Option<Rect>,
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
        page: None,
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
        // A page scrolls the window's content under its heading; a list inside it scrolls rows
        // the page already scrolls.
        let under_page = self.page;
        if node.widget == "Page" {
            self.page = Some(own);
        }
        let was_beyond = self.beyond;
        let was_across = self.across;
        let was_off_pixel = self.off_pixel;
        if geometric {
            self.pixels(node, keys, own);
            self.bare_row(node, keys, own);
            self.page_in_page(node, keys);
            self.lone_door(node, keys, above);
            self.empty_heading(node, keys, own);
            self.empty_surface(node, keys, own);
            if under_page.is_some() {
                self.nested_scroller(node, keys, own);
            }
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
        self.page = under_page;
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

    /// A page standing inside a page: the readable column is one thing, its gutter counted
    /// once, so a page given a column of its own inside one counts the gutter twice — what it
    /// holds stands inset from the rows beside it and narrower than them, as if the column had
    /// moved for that one row.
    fn page_in_page(&mut self, node: &InspectionNode, keys: &[String]) {
        if node.widget != PAGE {
            return;
        }
        let Some(inner) = held_page(node) else {
            return;
        };
        self.push(
            Kind::PageInPage,
            inner,
            keys,
            format!(
                "it is a page inside the page it stands in: its content is {:.0} narrower than the column's, inset by the page's gutter a second time",
                rect(node.rect).width() - rect(inner.rect).width()
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

    /// A list that scrolls rows a page above it already scrolls: one column scrolls in one place,
    /// so the list stands as tall as its rows and the page carries them past the window's edge.
    fn nested_scroller(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        if !super::ux::is_list(node) {
            return;
        }
        let Some(rows) = held(node) else {
            return;
        };
        if rows.y1 <= own.y1 + EPSILON {
            return;
        }
        self.push(
            Kind::NestedScroller,
            node,
            keys,
            format!(
                "its rows reach {:.0} px past the box it scrolls them in, while the page around it \
                 scrolls too",
                rows.y1 - own.y1
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
