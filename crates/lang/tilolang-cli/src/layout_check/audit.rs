//! What is wrong with one laid-out window: read off the toolkit's inspection of the frame (its
//! widgets' boxes, clips and semantics) and, for text, the widgets' own measurements.
//!
//! The bar's own layout is [`bar`]'s, and what text measures against its box is [`text`]'s.

mod bar;
mod kind;
mod text;
mod words;

pub use bar::{bar, bar_primary, column_headers_at_rest, offered_twice, stacked_header_rows};
pub use kind::Kind;
pub use text::clipped_text;
use text::header_row_problem;
#[cfg(test)]
use text::{reach_past, title_cut_with_room};
pub use words::text_on_text;

#[cfg(test)]
mod tests;

use ui::headless::InspectionNode;
use ui::kurbo::Rect;
use ui::widget::band_height;

#[derive(Clone, Debug)]
pub struct Problem {
    pub kind: Kind,
    pub keys: Vec<String>,
    pub widget: String,
    pub id: Option<String>,
    pub detail: String,
}

const EPSILON: f64 = 1.0;

/// How far from a device pixel an edge may stand, in device pixels: what the toolkit's single
/// precision coordinates carry, far under anything a person could see.
const PIXEL_SLACK: f64 = 0.01;

/// The tail of a page's "…" test id: the door the host
/// gives a page's title the actions for its item
/// behind.
const DOOR: &str = "~more";

/// A line's widget: its children side by side, where a door belongs at the trailing edge.
const LINE: &str = "Row";

/// A heading's widget: the band a section goes by, over what the section holds.
const HEADING: &str = "SectionHeader";

/// What the accessibility target naming a list's group heading starts with: the rest is the
/// heading, as the toolkit hands it to the group it heads.
const GROUP: &str = "group:";

/// A section's widget: the groups a page is made of, each under its heading.
const SECTION: &str = "Section";

/// The writing a page holds: the editable text a
/// person writes, which the page's own sections stand
/// after.
const WRITING: &str = "BlockEditor";

/// A rule's widget: the line that says one part of a page ends and the next begins.
const RULE: &str = "Divider";

const PAGE: &str = "Page";

/// The shell's way back, and its sidebar toggle: the two controls a bar navigates with.
const BACK: &str = "nav-shell-back";
const SIDEBAR_TOGGLE: &str = "nav-shell-sidebar";

const SHELL: &str = "NavShell";

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

fn stands(widget: &str, r: Rect) -> bool {
    if widget == RULE {
        r.width() > EPSILON && r.height() > 0.0
    } else {
        has_area(r)
    }
}

/// Whether a box, or anything it holds, gives a screen reader words to announce.
fn has_words(node: &InspectionNode) -> bool {
    node.semantics
        .as_ref()
        .and_then(|access| access.name.as_deref())
        .is_some_and(|name| !name.trim().is_empty())
        || node.children.iter().any(has_words)
}

fn class_name(class: ui::widget::arrange::WidthClass) -> &'static str {
    match class {
        ui::widget::arrange::WidthClass::Compact => "a phone's screen",
        ui::widget::arrange::WidthClass::Medium => "a column",
        ui::widget::arrange::WidthClass::Expanded => "an expanded column",
        ui::widget::arrange::WidthClass::Wide => "a wide column",
    }
}

fn size(r: Rect) -> String {
    format!("{:.0}×{:.0}", r.width(), r.height())
}

/// `text` as one line of words: whitespace collapsed, ends trimmed.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the row's fact `fact` says the day `heading` says: the day's own words, and the time of
/// day it is due after them where it names one.
fn says_day(fact: &str, heading: &str, days: &[String]) -> bool {
    if !days.iter().any(|day| day.eq_ignore_ascii_case(heading)) {
        return false;
    }
    let fact = fact.trim();
    fact.eq_ignore_ascii_case(heading)
        || (fact
            .get(..heading.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(heading))
            && fact
                .get(heading.len()..)
                .is_some_and(|rest| rest.starts_with(' ')))
}

/// The words each day around today reads as a heading (`Today`, `Saturday`, `Jun 29`, `Overdue
/// since Jun 29`): what a day heading over rows says.
fn day_words() -> Vec<String> {
    let today = ui::clock::today();
    (-400..=400)
        .filter_map(|days| today.checked_add_signed(chrono::Duration::days(days)))
        .map(|day| ui::format::day_fact_today(day, None).text)
        .collect()
}

/// The words a widget reads over what it holds: a card's or
/// a section's title, or the group a list heads its rows by.
fn heading_words(node: &InspectionNode) -> Option<String> {
    if matches!(node.widget.as_str(), "Card" | SECTION) {
        return node
            .children
            .iter()
            .find(|child| child.widget == HEADING)
            .map(words_of)
            .filter(|said| !said.is_empty());
    }
    let access = node.semantics.as_ref()?;
    access
        .children
        .iter()
        .find(|child| {
            child
                .action_target
                .as_deref()
                .is_some_and(|target| target.starts_with(GROUP))
        })
        .and_then(|heading| heading.name.as_deref())
        .map(words)
        .filter(|said| !said.is_empty())
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Scrolls {
    down: bool,
    /// Content may go on past its sides (a board's lanes in `ui.Scroll(across: true)`).
    across: bool,
}

/// Which ways `node` scrolls its content: as a scroll view says of its axis, else down for a widget
/// that scrolls by its kind (a long list).
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

/// Whether a node is one row of a list: what a list draws per item, which a surface of its own (a
/// result, a notice) draws too.
fn is_row(node: &InspectionNode) -> bool {
    matches!(
        node.widget.as_str(),
        "ListRow" | "TreeViewRow" | "SwipeRow" | "ListItem"
    ) || node
        .semantics
        .as_ref()
        .is_some_and(|access| access.role == ui::a11y::Role::ListItem)
}

/// The names of the rows every list and tree a box
/// holds shows, at any depth: what a document
/// lists.
fn held_rows(node: &InspectionNode, out: &mut Vec<String>) {
    let holds_rows = node.semantics.as_ref().is_some_and(|semantics| {
        matches!(semantics.role, ui::a11y::Role::List | ui::a11y::Role::Tree)
    });
    for child in &node.children {
        if holds_rows
            && is_row(child)
            && let Some(name) = child
                .semantics
                .as_ref()
                .and_then(|semantics| semantics.name.clone())
                .filter(|name| !name.trim().is_empty())
        {
            out.push(name);
        }
        held_rows(child, out);
    }
}

/// Whether a node is one row of a list of items: what a list draws per item.
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

/// Whether a pane takes the room of a column: more of
/// the window than a notice standing in what it says.
fn pane_sized(pane: Rect, window: Rect) -> bool {
    has_area(pane)
        && pane.width() >= window.width() * PANE_WIDTH
        && pane.height() >= window.height() * PANE_HEIGHT
}

/// Whether a widget hangs part of itself outside the column it is in by design: a document's block
/// editor hangs its handle gutter in the margin, so its text lines up with the title above it.
fn hangs(widget: &str) -> bool {
    matches!(widget, "BlockEditor")
}

/// Whether a widget floats over the content it belongs to, shown only on hover or focus (a row's
/// hover toolbar over its trailing top corner): neither overflow nor overlap.
fn floats(widget: &str) -> bool {
    matches!(widget, "HoverToolbarFace")
}

/// Whether a widget the window's top layer paints: it stands against the window it floats in, not
/// the box that opened it, so it neither widens what its owner holds nor reaches past it.
fn floats_in_window(widget: &str) -> bool {
    matches!(widget, "ContextMenu" | "Tooltip" | "Popover")
}

fn held(node: &InspectionNode) -> Option<Rect> {
    node.children
        .iter()
        .filter(|child| !floats(&child.widget) && !floats_in_window(&child.widget))
        .map(|child| rect(child.rect))
        .filter(|r| has_area(*r))
        .reduce(|a, b| a.union(b))
}

/// The first page a node holds, at any depth through what it
/// lays out: a page given a column of its own inside another
/// page.
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

/// The boxes of the text `node` shows, its own and its descendants', each as much of it as can be
/// seen: what a scroller or a clip of theirs hides is not read, so it covers nothing, and what
/// floats over everything (a menu, a tooltip) is no text of the box it opens from.
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

/// The boxes of the headings `node` shows, its own and its descendants', each as much of it as
/// can be seen: the words a surface goes by.
fn heading_boxes(node: &InspectionNode, clip: Rect, out: &mut Vec<Rect>) {
    use ui::a11y::Role;
    if floats(&node.widget) {
        return;
    }
    let inner = node
        .child_clip
        .map_or(clip, |child_clip| clip.intersect(rect(child_clip)));
    let heading = node
        .semantics
        .as_ref()
        .is_some_and(|access| matches!(access.role, Role::Heading { .. }));
    if heading {
        let seen = rect(node.rect).intersect(clip);
        if has_area(seen) {
            out.push(seen);
        }
        return;
    }
    for child in &node.children {
        heading_boxes(child, inner, out);
    }
}

/// Whether `node`, or anything it holds, gives a person something to read or to press: the words a
/// box paints itself, a control, or what stands inside it.
fn shows_anything(node: &InspectionNode) -> bool {
    if floats(&node.widget) {
        return false;
    }
    if is_text(node)
        || node.semantics.as_ref().is_some_and(|access| {
            !access.state.hidden
                && (access
                    .name
                    .as_deref()
                    .is_some_and(|name| !name.trim().is_empty())
                    || interactive(access.role))
        })
    {
        return true;
    }
    node.children.iter().any(shows_anything)
}

/// The title of a surface that says what it is for and shows nothing under it, with the band that
/// title stands in: the room below the band holds no words and no control, so a person reads the
/// name of a thing that is not there.
fn shows_nothing_under_its_title(node: &InspectionNode, own: Rect) -> Option<(Rect, Rect)> {
    if !paints_surface(&node.widget) {
        return None;
    }
    // A band folded away says nothing of what it holds until it opens.
    if node
        .semantics
        .as_ref()
        .is_some_and(|access| access.state.expanded == Some(false))
    {
        return None;
    }
    let mut headings = Vec::new();
    heading_boxes(node, own, &mut headings);
    let title = headings.first().copied()?;
    // The surface's own parts, as it lays them out: the band its header stands in — what overlaps
    // the first part — and everything the surface puts under that band.
    let standing: Vec<&InspectionNode> = node
        .children
        .iter()
        .filter(|child| !floats(&child.widget))
        .filter(|child| has_area(rect(child.rect).intersect(own)))
        .collect();
    let first = standing.first().map(|child| rect(child.rect))?;
    let band = standing
        .iter()
        .map(|child| rect(child.rect))
        .filter(|part| part.y0 < first.y1 - EPSILON)
        .fold(first, |band, part| band.union(part));
    if standing
        .iter()
        .filter(|child| rect(child.rect).y0 >= band.y1 - EPSILON)
        .any(|child| shows_anything(child))
    {
        return None;
    }
    // A surface only as tall as what it says is a surface of its words, with no body to be empty:
    // the fault is a title given room under it and nothing to put there.
    (own.y1 - band.y1 >= CARD_GAP).then_some((title, band))
}

/// Whether some text of `a` and some text of `b` stand on each other, so
/// neither can be read: two boxes laid over each other on purpose (a dock
/// over the desk under the bar) whose text stands apart can both be read.
fn texts_meet(a: &InspectionNode, b: &InspectionNode, clip: Rect) -> bool {
    let (mut under, mut over) = (Vec::new(), Vec::new());
    text_boxes(a, clip, &mut under);
    text_boxes(b, clip, &mut over);
    under
        .iter()
        .any(|u| over.iter().any(|o| has_area(u.intersect(*o))))
}

/// Whether `node` is a window standing on a desktop, or holds only one: its surface is opaque and
/// drawn over what it stands on, so what it covers is behind it by design, as on every desktop, and
/// nothing of the two is read through the other.
fn stands_as_window(node: &InspectionNode) -> bool {
    node.widget == "WindowFrame" || matches!(&node.children[..], [only] if stands_as_window(only))
}

/// Whether `node` is a surface floating over what it covers on its own opaque ground: a panel or a
/// popover over the desk is read on its own, as a window on a desktop is, so what stands behind it
/// is behind it by design.
fn floats_opaque(node: &InspectionNode) -> bool {
    matches!(
        node.widget.as_str(),
        "FloatingPanel" | "Popover" | "ContextMenu" | "Tooltip"
    ) || matches!(&node.children[..], [only] if floats_opaque(only))
}

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
/// which shows what it holds.
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

/// Whether `widget` paints a surface with rounded corners: a card or a notice whose shape is a
/// rounded well, so an edge of the column or window cutting it cuts the shape.
fn rounds_its_corners(widget: &str) -> bool {
    matches!(widget, "Banner" | "Card")
}

/// The least gap a card keeps from what stands over or under it: the toolkit's sibling gap, the
/// rhythm cards and rows of a list stand by (`rhythm.sibling` on the stock theme).
const CARD_GAP: f64 = 12.0;

/// How much more room a heading keeps over it than over its content: the toolkit's tightest step
/// (`rhythm.tight`), the least that tells a title from the block above it.
const HEADING_LEAD: f64 = 4.0;

/// Whether a node is one control a person presses: its own role says so, or it is a box (a header
/// action's frame) around exactly one.
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
    beyond: bool,
    /// Inside a view that scrolls sideways: its content reaches past the window by design.
    across: bool,
    min_target: Option<f64>,
    display_scale: f64,
    /// Inside a box already found off the device pixels: what it holds moves with it.
    off_pixel: bool,
    page: Option<Rect>,
    /// The column of the window's shell the node stands in:
    /// the box a shell gives one of its parts, beside the
    /// others.
    column: Option<Rect>,
    header_counts: Vec<Count>,
    place_counts: Vec<Count>,
    header_titles: Vec<Title>,
    page_titles: Vec<Title>,
    navigations: Vec<Navigation>,
    group_headings: Vec<GroupHeading>,
    heading: Option<String>,
    days: Vec<String>,
    out: &'a mut Vec<Problem>,
}

/// Words a window shows as a title: a column header's, or one its content shows as the page's.
struct Title {
    words: String,
    count: Option<u32>,
    at: Rect,
    keys: Vec<String>,
    widget: String,
    id: Option<String>,
}

struct Count {
    number: u32,
    at: Rect,
    keys: Vec<String>,
    widget: String,
    id: Option<String>,
}

/// The one group heading a list draws: what it says, and how many rows it heads.
struct GroupHeading {
    words: String,
    count: u32,
    at: Rect,
    keys: Vec<String>,
    widget: String,
    id: Option<String>,
}

/// A navigation control the window's bar shows: the way back, or the sidebar's toggle.
struct Navigation {
    back: bool,
    at: Rect,
    keys: Vec<String>,
    widget: String,
    id: Option<String>,
}

/// Every problem of the frame `root`, laid out in `viewport`, where a target needs `min_target`
/// logical pixels on each side.
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
        column: None,
        header_counts: Vec::new(),
        place_counts: Vec::new(),
        header_titles: Vec::new(),
        page_titles: Vec::new(),
        navigations: Vec::new(),
        group_headings: Vec::new(),
        heading: None,
        days: day_words(),
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
    walk.repeated_group_titles();
    walk.repeated_titles();
    walk.double_navigation();
    walk.placeholder_panes(root, &mut Vec::new());
    for fault in ui::presence::tag_faults(root) {
        out.push(Problem {
            kind: Kind::PresenceTag,
            keys: Vec::new(),
            widget: ui::presence::TAG_WIDGET.to_owned(),
            id: None,
            detail: fault,
        });
    }
    out
}

/// The room every group heading takes over its first row: one heading band, grown only to a
/// finger's target where the window is held in the hand, and nothing else.
pub fn heading_rooms(
    root: &InspectionNode,
    theme: &ui::Theme,
    min_target: Option<f64>,
) -> Vec<Problem> {
    let band = band_height(theme).max(min_target.unwrap_or(0.0));
    let inset = ui::chrome::column_gutter(theme);
    let mut out = Vec::new();
    heading_rooms_in(root, &mut Vec::new(), band, inset, &mut out);
    out
}

fn heading_rooms_in(
    node: &InspectionNode,
    keys: &mut Vec<String>,
    band: f64,
    inset: f64,
    out: &mut Vec<Problem>,
) {
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    let headings = node
        .semantics
        .as_ref()
        .map_or(&[][..], |access| access.children.as_slice());
    for (at, heading) in headings.iter().enumerate() {
        // A group's heading is the list's disclosure of it: the one target it names a group by.
        let names_group = heading
            .action_target
            .as_deref()
            .is_some_and(|target| target.starts_with("group:"));
        let Some(bounds) = heading.bounds.filter(|_| names_group) else {
            continue;
        };
        let name = heading.name.clone().unwrap_or_default();
        let taken = bounds.y1 - bounds.y0;
        if (taken - band).abs() > 0.5 {
            out.push(Problem {
                kind: Kind::HeadingRoom,
                keys: keys.iter().rev().cloned().collect(),
                widget: node.widget.clone(),
                id: node.test_id.clone(),
                detail: format!(
                    "the heading {name:?} takes {taken:.0} px over its first row, where a \
                     heading band is {band:.0}"
                ),
            });
        }
        let Some(row) = at.checked_add(1).and_then(|next| headings.get(next)) else {
            continue;
        };
        let heads_another = row
            .action_target
            .as_deref()
            .is_some_and(|target| target.starts_with("group:"));
        let Some(row) = row.bounds.filter(|_| !heads_another) else {
            continue;
        };
        let gap = row.y0 - bounds.y1;
        if gap.abs() > 0.5 {
            out.push(Problem {
                kind: Kind::HeadingRoom,
                keys: keys.iter().rev().cloned().collect(),
                widget: node.widget.clone(),
                id: node.test_id.clone(),
                detail: format!(
                    "the row the heading {name:?} heads stands {gap:.0} px from its band's end"
                ),
            });
        }
    }
    // A list that heads its rows starts its first band on the column's
    // content gutter, where a page's first block stands: a heading
    // drawn there, or the first row where the list leaves that heading
    // out.
    if let Some(ui::widget::LayoutInfo::List(list)) = node.layout
        && list.heads_rows
    {
        // The band the person sees: the heading the list draws over its first run says where it
        // stands; a list that draws none says the room it keeps over its first row.
        let drawn = headings
            .iter()
            .find(|child| child.bounds.is_some())
            .filter(|heading| {
                heading
                    .action_target
                    .as_deref()
                    .is_some_and(|target| target.starts_with(GROUP))
            })
            .and_then(|heading| heading.bounds)
            .map(|bounds| bounds.y0 - rect(node.rect).y0);
        let room = drawn.unwrap_or_else(|| f64::from(list.lead));
        if room + 0.5 < inset {
            out.push(Problem {
                kind: Kind::HuggingRow,
                keys: keys.iter().rev().cloned().collect(),
                widget: node.widget.clone(),
                id: node.test_id.clone(),
                detail: format!(
                    "the list keeps {room:.0} px over the first band it draws, where a page's \
                     content gutter is {inset:.0}"
                ),
            });
        }
    }
    for child in &node.children {
        heading_rooms_in(child, keys, band, inset, out);
    }
    if pushed {
        keys.pop();
    }
}

/// The room a page's body keeps from the page's own gutter: the body of a
/// page whose heading leads with a control stands one row lead in, under
/// the text that control leads to — the control's cell and the gap after
/// it.
pub fn indented_bodies(
    root: &InspectionNode,
    theme: &ui::Theme,
    min_target: Option<f64>,
) -> Vec<Problem> {
    let target = min_target.unwrap_or_else(|| {
        f64::from(ui::interaction::InteractionEnvironment::POINTER.minimum_target())
    });
    let lead = target + f64::from(theme.tokens.spacing.rhythm.get(ui::tokens::Rhythm::Related));
    let rem = f64::from(ui::widget::arrange::rem_of(theme));
    let mut out = Vec::new();
    let above = rect(root.rect);
    indented_bodies_in(root, &mut Vec::new(), lead, rem, None, above, &mut out);
    out
}

/// Whether a column `width` logical pixels wide, at `rem` pixels to the text size, is a phone's
/// screen: narrower than the width class a column starts at.
fn phone_screen(width: f64, rem: f64) -> bool {
    let (_, column) = ui::widget::arrange::WidthClass::Compact.band();
    width / rem < f64::from(column)
}

fn indented_bodies_in(
    node: &InspectionNode,
    keys: &mut Vec<String>,
    lead: f64,
    rem: f64,
    page: Option<f64>,
    above: Rect,
    out: &mut Vec<Problem>,
) {
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    let own = rect(node.rect);
    // The gutter the page's content starts on: what its
    // blocks stand at, the header that leads them
    // included.
    let gutter = if node.widget == PAGE {
        node.children
            .iter()
            .map(|child| rect(child.rect))
            .filter(|child| has_area(*child))
            .map(|child| child.x0)
            .reduce(f64::min)
    } else {
        page
    };
    // A block standing one lead in from the box that holds it, that box on the page's own gutter:
    // what the page's body holds, not a cell a grid places far across the page.
    if let (Some(gutter), "Section") = (gutter, node.widget.as_str())
        && (own.x0 - above.x0 - lead).abs() <= 0.5
        && above.x0 <= gutter + 0.5
        && phone_screen(own.width(), rem)
    {
        out.push(Problem {
            kind: Kind::IndentedBody,
            keys: keys.iter().rev().cloned().collect(),
            widget: node.widget.clone(),
            id: node.test_id.clone(),
            detail: format!(
                "it starts {:.0} px in from the page's gutter at x {gutter:.0}, {:.0} wide: a \
                 page on a phone's screen starts its body on the gutter",
                own.x0 - gutter,
                own.width()
            ),
        });
    }
    let above = if has_area(own) { own } else { above };
    for child in &node.children {
        indented_bodies_in(child, keys, lead, rem, gutter, above, out);
    }
    if pushed {
        keys.pop();
    }
}

impl Walk<'_> {
    /// `parent`: the nearest box above with geometry.
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

    /// What is wrong with `node`'s own box, before what it holds is read: `above` the widget it
    /// stands in, `under_page` the page it scrolls in, when one is open over it.
    fn own_box(
        &mut self,
        node: &InspectionNode,
        keys: &[String],
        own: Rect,
        above: &str,
        under_page: Option<Rect>,
    ) {
        self.pixels(node, keys, own);
        self.bare_row(node, keys, own);
        self.page_in_page(node, keys);
        self.docked_column(node, keys);
        self.lone_door(node, keys, above);
        self.empty_heading(node, keys, own);
        self.empty_surface(node, keys, own);
        self.header_only(node, keys, own);
        self.rounded_on_edge(node, keys, own);
        self.repeated_day_fact(node, keys);
        if under_page.is_some() {
            self.nested_scroller(node, keys, own);
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
        // The box a scroll view holds its content in is that view, not a container of its own: what
        // it holds goes on past the view as the view allows, and what that content holds in turn is
        // checked against the content, not the view.
        let content = content && geometric && !scrolling;
        // What of the window is not shown at all needs no
        // checking: a column pushed aside, a row scrolled out of
        // view.
        if geometric && !has_area(own.intersect(clip)) {
            return;
        }
        let was_heading = self.heading_over(node);
        // A page scrolls the window's content under its heading;
        // a list inside it scrolls rows the page already scrolls.
        let under_page = self.page;
        if node.widget == "Page" {
            self.page = Some(own);
        }
        let (was_beyond, was_across, was_off_pixel) = (self.beyond, self.across, self.off_pixel);
        if geometric {
            self.own_box(node, keys, own, above, under_page);
        }
        if geometric && !hangs(&node.widget) {
            self.reach(node, keys, own, parent, scroller);
            if let Some(min) = self.min_target {
                self.target(node, keys, own, min);
            }
        }
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
            self.says(node, keys, own, inner_clip);
            self.header_rows(node, keys);
            self.content_beside_sidebar(node, keys);
            self.navigation(node, keys, own.intersect(inner_clip));
        }
        self.page_checks(node, keys);
        for child in &node.children {
            if floats(&child.widget) {
                continue;
            }
            // A menu the window's top layer paints stands against the window, not the box that opened
            // it: it is measured from the window's edges, and its own edges and targets are still
            // checked.
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
            // A scroll view holds its content in a box of its own,
            // which is the view's own content rather than a frame
            // around it.
            let was_column = self.column;
            if node.widget == SHELL && has_area(rect(child.rect)) {
                self.column = Some(rect(child.rect));
            }
            self.node(
                child,
                keys,
                inner_parent,
                inner_clip,
                inner_scroller,
                scrolling,
                &node.widget,
            );
            self.column = was_column;
        }
        self.beyond = was_beyond;
        self.across = was_across;
        self.off_pixel = was_off_pixel;
        self.page = under_page;
        self.heading = was_heading;
    }

    /// The checks of a page's own parts: cards standing cramped against a sibling, an item shown
    /// twice, a heading crowded or a body unruled.
    fn page_checks(&mut self, node: &InspectionNode, keys: &[String]) {
        self.cramped_card(node, keys);
        self.shown_twice(node, keys);
        self.heading_rhythm(node, keys);
        self.unruled_body(node, keys);
    }

    /// What the box `node` heads what it holds with, where it heads it — a card's or a section's
    /// title, or the group a list heads its rows by — as the words its children then stand under;
    /// the heading before it, to give back once they are read.
    fn heading_over(&mut self, node: &InspectionNode) -> Option<String> {
        let was = self.heading.clone();
        if let Some(said) = heading_words(node) {
            self.heading = Some(said);
        }
        was
    }

    /// A row's trailing day fact naming the day the heading over it names: the group, card or
    /// section above the rows already says the day, so a row saying it again is noise — what it
    /// keeps is the time of day it is due, and nothing where it names no time.
    fn repeated_day_fact(&mut self, node: &InspectionNode, keys: &[String]) {
        let Some(heading) = self.heading.as_deref() else {
            return;
        };
        let Some(access) = node.semantics.as_ref() else {
            return;
        };
        if access.role != ui::a11y::Role::ListItem {
            return;
        }
        let Some(value) = access.value.as_deref() else {
            return;
        };
        let Some(fact) = value
            .split(", ")
            .map(str::trim)
            .find(|fact| says_day(fact, heading, &self.days))
        else {
            return;
        };
        self.push(
            Kind::RepeatedDayFact,
            node,
            keys,
            format!("the row's fact {fact:?} says the day the heading over it says ({heading:?})"),
        );
    }

    /// A box with an edge between two device pixels: the outermost such box only, since what it
    /// holds is placed from its edges.
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

    /// A row stretched over the column it stands in: the page drawn as one row, its line floating
    /// at the column's middle where a page around it would hold it.
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

    /// A bar docked at the foot of a column stands in the column the content over it is read at, as
    /// wide as that column and its leading edge where the column's is: what adds to a list is an
    /// entry of the list, not a band across the window beside it.
    fn docked_column(&mut self, node: &InspectionNode, keys: &[String]) {
        if node.widget != "Docked" {
            return;
        }
        let Some(foot) = node.children.last().filter(|child| child.widget == PAGE) else {
            return;
        };
        let Some(content) = node.children.first() else {
            return;
        };
        // The column the content is read at is the one its page keeps, where it holds a page:
        // content that fills its column fills the bar's too.
        let (Some(column_page), Some(bar)) = (held_page(content), held(foot)) else {
            return;
        };
        let Some(column) = held(column_page) else {
            return;
        };
        // The content stands in the room a scroller keeps for its bar, which the bar at the foot
        // does not: a column the measure bounds centres in it, half that room to the other side,
        // and a column as wide as the viewport keeps its whole width of it at the trailing edge.
        let lane = (rect(content.rect).width() - rect(column_page.rect).width()).max(0.0);
        if (bar.width() - column.width()).abs() <= lane + 1.0
            && (bar.x0 - column.x0).abs() <= lane / 2.0 + 1.0
        {
            return;
        }
        self.push(
            Kind::DockedColumn,
            node,
            keys,
            format!(
                "the bar at the column's foot stands {:.0} wide from x {:.0}, where the content over it is read {:.0} wide from x {:.0}: an entry that adds to the column stands in the column, not across the window beside it",
                bar.width(),
                bar.x0,
                column.width(),
                column.x0
            ),
        );
    }

    /// A page standing inside a page: the readable column is one thing, its gutter counted once, so a
    /// page given a column of its own inside one counts the gutter twice — what it holds stands inset
    /// from the rows beside it and narrower than them, as if the column had moved for that one row.
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

    /// A page's "…" in a column: the door is the item's actions at the trailing edge of the title's
    /// line, so one standing in a column takes a line of the document for its glyph alone, under
    /// the title and out of reach of the page's own composition.
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

    /// A heading with nothing in it: it takes the band a section goes by, and the content it stands
    /// over starts below an empty strip.
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
    /// page, and no words stand in it and nothing of its own is laid out.
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

    /// A panel or a sheet that says what it is for and holds nothing under its title: the person
    /// reads the name of what they opened and finds no control and no word of the thing itself.
    fn header_only(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        let Some((title, _)) = shows_nothing_under_its_title(node, own) else {
            return;
        };
        self.push(
            Kind::HeaderOnly,
            node,
            keys,
            format!(
                "it says what it shows in {} of words at its top, in a surface {} tall, and everything under them is empty",
                size(title),
                size(own).rsplit('×').next().unwrap_or_default()
            ),
        );
    }

    /// A card or a notice standing closer to what stands over or under it than the gap siblings keep:
    /// the two read as one block, and a list pressed against a notice says the notice is a row of it.
    fn cramped_card(&mut self, node: &InspectionNode, keys: &[String]) {
        let standing: Vec<&InspectionNode> = node
            .children
            .iter()
            .filter(|child| has_area(rect(child.rect)) && !floats(&child.widget))
            .collect();
        for (index, card) in standing.iter().enumerate() {
            if !rounds_its_corners(&card.widget) {
                continue;
            }
            let own = rect(card.rect);
            let above = index.checked_sub(1).and_then(|before| standing.get(before));
            let below = standing.get(index.saturating_add(1));
            for (side, sibling) in [("above", above), ("below", below)] {
                let Some(sibling) = sibling else {
                    continue;
                };
                let other = rect(sibling.rect);
                // A box beside it, in a row or a grid, is no neighbour over or under it; one
                // reaching past its sides with words of its own is no column's sibling either.
                if other.x1 <= own.x0 + EPSILON || other.x0 >= own.x1 - EPSILON {
                    continue;
                }
                let gap = if side == "above" {
                    if other.y1 > own.y0 + EPSILON {
                        continue;
                    }
                    own.y0 - other.y1
                } else {
                    if other.y0 < own.y1 - EPSILON {
                        continue;
                    }
                    other.y0 - own.y1
                };
                if gap >= CARD_GAP - EPSILON {
                    continue;
                }
                let mut card_keys = keys.to_vec();
                if let Some(id) = card.test_id.clone() {
                    card_keys.push(id);
                }
                self.push(
                    Kind::CrampedCard,
                    card,
                    &card_keys,
                    format!(
                        "it stands {gap:.0} px from the {} {side} it: a card keeps a sibling gap \
                         ({CARD_GAP:.0} px)",
                        sibling.widget
                    ),
                );
            }
        }
    }

    /// One item shown twice: a row standing open into its item draws a list of what it holds, and
    /// its own list shows those again as rows.
    fn shown_twice(&mut self, node: &InspectionNode, keys: &[String]) {
        let rows: Vec<&InspectionNode> =
            node.children.iter().filter(|child| is_row(child)).collect();
        if rows.len() < 2 {
            return;
        }
        let titled = |row: &InspectionNode| {
            row.semantics
                .as_ref()
                .and_then(|semantics| semantics.name.clone())
                .filter(|name| !name.trim().is_empty())
        };
        let listed: Vec<String> = rows.iter().filter_map(|row| titled(row)).collect();
        for row in &rows {
            // A row standing open is expanded and holds rows: what a folded branch's row says of
            // itself says nothing of an item's document.
            let open = row
                .semantics
                .as_ref()
                .is_some_and(|semantics| semantics.state.expanded == Some(true));
            if !open {
                continue;
            }
            let mut held = Vec::new();
            held_rows(row, &mut held);
            for item in held {
                if !listed.contains(&item) {
                    continue;
                }
                let title = titled(row).unwrap_or_default();
                self.push(
                    Kind::ItemShownTwice,
                    row,
                    keys,
                    format!(
                        "the document of the row standing open into \"{title}\" lists \"{item}\", \
                         which stands in the list again: an item is shown once"
                    ),
                );
            }
        }
    }

    /// A section's heading standing no further from the block over it than from the content it
    /// opens: a heading belongs to what follows it, so it keeps clearly more room under the block
    /// it stands below than over its own content.
    fn heading_rhythm(&mut self, node: &InspectionNode, keys: &[String]) {
        // A page's blocks read at the page's rhythm; a
        // section inside a card or a panel is the box's to
        // space.
        if self.page.is_none() {
            return;
        }
        let standing: Vec<&InspectionNode> = node
            .children
            .iter()
            .filter(|child| has_area(rect(child.rect)) && !floats(&child.widget))
            .collect();
        for (index, section) in standing.iter().enumerate() {
            let Some(header) = section
                .children
                .first()
                .filter(|child| child.widget == HEADING)
            else {
                continue;
            };
            let Some(first) = section
                .children
                .iter()
                .skip(1)
                .find(|child| has_area(rect(child.rect)) && !floats(&child.widget))
            else {
                continue;
            };
            // A heading first in its box has nothing over it to stand apart from.
            let Some(over) = index.checked_sub(1).and_then(|before| standing.get(before)) else {
                continue;
            };
            let above = rect(section.rect).y0 - rect(over.rect).y1;
            let below = rect(first.rect).y0 - rect(header.rect).y1;
            if above >= below + HEADING_LEAD - EPSILON {
                continue;
            }
            let mut told = keys.to_vec();
            if let Some(id) = section.test_id.clone() {
                told.push(id);
            }
            self.push(
                Kind::CrowdedHeading,
                header,
                &told,
                format!(
                    "it stands {above:.0} px under what comes before it and {below:.0} px over \
                     its own content: a heading keeps clearly more room over it than under it, \
                     so it reads with what it opens"
                ),
            );
        }
    }

    /// A page's own sections standing straight under the writing it holds: a section's heading is
    /// set like the writing's headings and stands no further from the writing than they stand from
    /// each other, so what the page says of itself reads as one more line of what was written.
    fn unruled_body(&mut self, node: &InspectionNode, keys: &[String]) {
        // A page's writing and its own parts read at the page's
        // rhythm; a section inside a card is the box's to space.
        if self.page.is_none() {
            return;
        }
        let standing: Vec<&InspectionNode> = node
            .children
            .iter()
            .filter(|child| stands(&child.widget, rect(child.rect)) && !floats(&child.widget))
            .collect();
        for (index, child) in standing.iter().enumerate() {
            if child.widget != WRITING {
                continue;
            }
            let Some(next) = index.checked_add(1).and_then(|after| standing.get(after)) else {
                continue;
            };
            // What a person reads straight after the writing: the
            // box that follows it, or the first block in that box.
            let opens = if next.widget == SECTION {
                *next
            } else {
                let Some(first) = next.children.iter().find(|child| {
                    stands(&child.widget, rect(child.rect)) && !floats(&child.widget)
                }) else {
                    continue;
                };
                first
            };
            if opens.widget != SECTION {
                continue;
            }
            let at = opens
                .children
                .iter()
                .find(|child| child.widget == HEADING)
                .unwrap_or(opens);
            let mut told = keys.to_vec();
            if let Some(id) = opens.test_id.clone() {
                told.push(id);
            }
            self.push(
                Kind::UnruledBody,
                at,
                &told,
                "it stands straight under the writing, with nothing between them: a rule over the \
                 page's own sections says where the writing ends, so its headings never read as \
                 another line of the text"
                    .to_owned(),
            );
        }
    }

    /// A surface with rounded corners standing on the edge of the column it is in, or of the window
    /// where no column holds it: the edge cuts its curve, and the surface reads as part of the
    /// frame rather than as a card inside it.
    fn rounded_on_edge(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        if !rounds_its_corners(&node.widget) {
            return;
        }
        let inside = self.column.unwrap_or(self.viewport);
        let scrolls = self.page.is_some();
        let edges = [
            ("left", own.x0 - inside.x0, false),
            ("right", own.x1 - inside.x1, false),
            ("top", own.y0 - inside.y0, true),
            ("bottom", own.y1 - inside.y1, true),
        ];
        let mut on: Vec<&str> = edges
            .into_iter()
            .filter(|(_, _, vertical)| !(scrolls && *vertical))
            .filter(|(_, gap, _)| gap.abs() <= EPSILON)
            .map(|(side, _, _)| side)
            .collect();
        if on.is_empty() {
            return;
        }
        let where_ = if self.column.is_some() {
            "its column"
        } else {
            "the window"
        };
        on.sort_unstable();
        self.push(
            Kind::RoundedOnEdge,
            node,
            keys,
            format!(
                "its {} {} stand{} on {} {} at ({:.0}, {:.0}): a rounded surface keeps a gutter",
                on.join(" and "),
                if on.len() == 1 { "edge" } else { "edges" },
                if on.len() == 1 { "s" } else { "" },
                where_,
                size(inside),
                inside.x0,
                inside.y0
            ),
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

    /// A scroll view with no room on the way it scrolls, holding more than that: nothing under it
    /// can be scrolled into view, so a card or a line the person was promised is gone.
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

    /// What `node` itself says, before what it holds is read: its titles and the numbers beside
    /// them, and the one group heading it draws over the rows of a list.
    fn says(&mut self, node: &InspectionNode, keys: &[String], own: Rect, clip: Rect) {
        self.counts(node, keys, own, clip);
        self.titles(node, keys, own);
        self.group_heading(node, keys);
    }

    /// The numbers `node` draws: a column header's count, or the counts of the places a list shows.
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
                        count: number(access.value.as_deref()).filter(|count| *count > 0),
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

    /// A column's header row as the shell settled it: the row holds its title against its actions by
    /// one rule at every width and in every language — the actions give way first, a label for its icon
    /// and then a place in the row for the menu's door, and the row never drops the app's primary
    /// action.
    fn header_rows(&mut self, node: &InspectionNode, keys: &[String]) {
        let Some(ui::widget::LayoutInfo::Shell(shell)) = &node.layout else {
            return;
        };
        for row in shell.rows.rows.iter().flatten() {
            if let Some(detail) = header_row_problem(row) {
                self.push(Kind::TitleClippedWithRoom, node, keys, detail);
            }
        }
    }

    /// A sidebar standing beside a pane narrower than the class its content asks for
    /// ([`ui::widget::ShellLayout::needs`]): the pane beside it is a phone's screen, where the
    /// sidebar folds behind the bar's toggle and the content takes the window instead.
    fn content_beside_sidebar(&mut self, node: &InspectionNode, keys: &[String]) {
        let Some(ui::widget::LayoutInfo::Shell(shell)) = &node.layout else {
            return;
        };
        if shell.sidebar != ui::widget::SidebarStanding::Docked
            || shell.beside >= shell.needs
            || shell.beside_width <= 0.0
        {
            return;
        }
        self.push(
            Kind::ContentBesideSidebar,
            node,
            keys,
            format!(
                "the content stands {:.0} wide beside the sidebar, {} where it asks for {}: the \
                 sidebar folds behind the bar's toggle there, and the content takes the window",
                shell.beside_width,
                class_name(shell.beside),
                class_name(shell.needs)
            ),
        );
    }

    /// A column's header count that a place beside it, to its
    /// left, draws too: the same number twice on one screen.
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

    /// The words `node` shows as a title of its own:
    /// the page's own heading, as the toolkit names
    /// it.
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
            count: None,
            at: own,
            keys: keys.iter().rev().cloned().collect(),
            widget: node.widget.clone(),
            id: node.test_id.clone(),
        });
    }

    /// The one group heading `node` draws, where it draws exactly one: what it says, how many rows
    /// it heads, and the list it stands in.
    fn group_heading(&mut self, node: &InspectionNode, keys: &[String]) {
        let Some(access) = node.semantics.as_ref() else {
            return;
        };
        let heads_group = |child: &ui::a11y::AccessNode| {
            child
                .action_target
                .as_deref()
                .is_some_and(|target| target.starts_with(GROUP))
        };
        let headings: Vec<usize> = access
            .children
            .iter()
            .enumerate()
            .filter(|(_, child)| heads_group(child))
            .map(|(at, _)| at)
            .collect();
        // Two groups tell the runs apart, and a heading says something the title does not.
        let [at] = headings[..] else {
            return;
        };
        let Some(heading) = access.children.get(at) else {
            return;
        };
        let (Some(name), Some(count), Some(bounds)) = (
            heading.name.clone(),
            heading
                .value
                .as_deref()
                .and_then(|value| value.trim().parse::<u32>().ok()),
            heading.bounds.map(ui::a11y::AccessBounds::to_rect),
        ) else {
            return;
        };
        // The rows still to do under the heading: a heading counts what is left, so a row the
        // person has finished with stands under it without counting.
        let left = access
            .children
            .iter()
            .skip(at.saturating_add(1))
            .take_while(|child| !heads_group(child))
            .filter(|child| child.role == ui::a11y::Role::ListItem)
            .filter(|child| child.state.checked != Some(true))
            .count();
        if left != usize::try_from(count).unwrap_or(0) {
            return;
        }
        self.group_headings.push(GroupHeading {
            words: words(&name),
            count,
            at: bounds,
            keys: keys.iter().rev().cloned().collect(),
            widget: node.widget.clone(),
            id: node.test_id.clone(),
        });
    }

    /// A list's group heading saying what the title over the list already says — the same words,
    /// counting the same rows: the title names where the person is and how much stands there, so
    /// the heading over the first row says nothing twice.
    fn repeated_group_titles(&mut self) {
        let groups = std::mem::take(&mut self.group_headings);
        for group in groups {
            let repeats = self.header_titles.iter().any(|title| {
                title.words.eq_ignore_ascii_case(&group.words)
                    && title.count == Some(group.count)
                    && group.at.y0 >= title.at.y1 - EPSILON
            });
            if !repeats {
                continue;
            }
            self.out.push(Problem {
                kind: Kind::RepeatedGroupTitle,
                keys: group.keys.clone(),
                widget: group.widget.clone(),
                id: group.id.clone(),
                detail: format!(
                    "the list's one group is headed {:?} over {} rows, where the title above it \
                     already reads the same",
                    group.words, group.count
                ),
            });
        }
    }

    /// A bar naming a page while the page shows those same words as its own title: the bar keeps
    /// only the way back and the page's actions until the title scrolls out of the page.
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

    /// The shell's own navigation controls, where the
    /// window shows them: its way back, and its sidebar
    /// toggle.
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

    /// A back control standing beside the sidebar toggle, in the same bar row: the window's leading
    /// corner offers two ways to navigate, where a screen that went somewhere deeper has one, its
    /// way back at the leading edge.
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

    /// A pane that only says nothing is chosen, standing beside the list the window chooses from:
    /// the pane takes the room the list would list in, and there is nothing in it to read.
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

    /// Children of a box that lays them out side by side or in turn, covering each other.
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
                let covers = if stands_as_window(b) || floats_opaque(b) {
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
