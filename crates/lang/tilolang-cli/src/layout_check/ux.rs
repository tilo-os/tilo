//! What a person sees wrong on a laid-out screen that its geometry alone calls fine: a list whose
//! rows stand on different columns, one choice filled twice, the same add affordance standing
//! several times over, a name a screen reader reads as code, a destructive verb of its own, a card
//! inside a card.
//!
//! Every rule is read off the inspection alone — the retained boxes, their clips, the grid each row
//! reports and the semantics a screen reader is given — so no app has to know about it, and one
//! that composes its screen differently is held to the same rules as every other. The one thing
//! the inspection cannot tell is what a control was declared to do: a field's clear button and one
//! that deletes an item are drawn alike, and only the view that sent it says which it is
//! ([`Declared`], read off the tree the app sent beside the inspection).

use std::collections::{BTreeMap, BTreeSet};

use tilo_ui_host::{UiNode, Value};
use ui::a11y::Role;
use ui::headless::InspectionNode;
use ui::kurbo::Rect;
use ui::widget::{LayoutInfo, RowLayout};

use super::audit::{Kind, Problem};

/// How far out of line two edges may stand, in logical pixels: layout places them on fractions of
/// a pixel, which a person does not see.
const SLACK: f64 = 1.0;

/// The nesting step the stock theme indents a row by, for the tests that lay rows out by hand.
#[cfg(test)]
const TOOLKIT_STEP: f64 = 22.0;

/// The room a screen keeps between its content and the window's edge on the narrowest window it is
/// made for.
pub const COMPACT_GUTTER: f64 = 16.0;

/// The window width under which a screen has no room for a page's own gutters and must keep the one
/// gutter itself.
const COMPACT_WIDTH: f64 = 600.0;

/// The words a screen's copy starts an add control with.
const ADD_VERBS: [&str; 3] = ["add ", "new ", "create "];

/// The words a screen's copy destroys with, other than the one verb every screen destroys with.
/// `clear` is among them only on a control the view declares destructive: a field's own clear
/// takes back what was typed, and keeps its word.
const OTHER_DELETE_VERBS: [&str; 7] = [
    "remove", "erase", "discard", "purge", "wipe", "drop", "clear",
];

/// The one verb a destructive control says its action with.
pub const DELETE_VERB: &str = "Delete";

/// What a view declares its controls do, where the inspection cannot tell: the controls it sends
/// that destroy what they name, by the key each is drawn with.
#[derive(Debug, Default)]
pub struct Declared {
    destructive: BTreeSet<String>,
}

impl Declared {
    /// Read the tree `tree` a view sent, and every tree under it.
    pub fn read(&mut self, tree: &UiNode) {
        if destroys(tree) {
            self.destructive.insert(tree.key.clone());
        }
        for child in &tree.children {
            self.read(child);
        }
    }

    /// Whether the control drawn as `key` was declared to destroy what it names.
    #[must_use]
    pub fn destroys(&self, key: &str) -> bool {
        self.destructive.contains(key)
    }
}

/// Whether `node` is a control its view declared destructive: an action's control
/// (`ui.MenuItem`, `ui.SwipeAction`) says so in a flag, a button in its role. A control that
/// clears a field's value declares no role and destroys nothing.
fn destroys(node: &UiNode) -> bool {
    let flagged = match node.props.get("destructive") {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Int(count)) => *count != 0,
        _ => false,
    };
    flagged || node.props.get("role") == Some(&Value::text("destructive"))
}

/// What this screen holds that a person would see wrong though its geometry is sound, as the
/// views that drew it declared what their controls do. `step` is the toolkit's nesting step: how
/// far one depth indents its content, and so the one step a first level may stand in by.
pub fn inspect(
    root: &InspectionNode,
    viewport: (u32, u32),
    declared: &Declared,
    step: f64,
) -> Vec<Problem> {
    let mut out = Vec::new();
    columns(root, &mut out);
    first_level(root, None, step, &mut Vec::new(), &mut out);
    selections(root, &mut Vec::new(), false, &mut out);
    chosen(root, &[], &mut Vec::new(), &mut out);
    adds(root, &mut Vec::new(), false, &mut out);
    names(root, &mut Vec::new(), &mut out);
    verbs(root, declared, &mut Vec::new(), &mut out);
    cards(root, &mut Vec::new(), false, &mut out);
    opens(root, None, &mut BTreeSet::new(), &mut Vec::new(), &mut out);
    repeated_row_titles(root, &mut Vec::new(), &mut out);
    gutter(root, f64::from(viewport.0), &mut Vec::new(), &mut out);
    out
}

/// Record one problem at `node`, with the views above it that a view's nodes made, as
/// [`super::audit`] records its own.
fn push(
    out: &mut Vec<Problem>,
    kind: Kind,
    node: &InspectionNode,
    keys: &[String],
    detail: String,
) {
    out.push(Problem {
        kind,
        keys: keys.iter().rev().cloned().collect(),
        widget: node.widget.clone(),
        id: node.test_id.clone(),
        detail,
    });
}

fn rect(node: &InspectionNode) -> Rect {
    Rect::new(
        node.rect[0],
        node.rect[1],
        node.rect[0] + node.rect[2],
        node.rect[1] + node.rect[3],
    )
}

fn semantics_name(node: &InspectionNode) -> Option<&str> {
    node.semantics.as_ref()?.name.as_deref()
}

fn role(node: &InspectionNode) -> Option<Role> {
    node.semantics.as_ref().map(|semantics| semantics.role)
}

/// The grid a row laid itself out on, where the node is one.
fn grid(node: &InspectionNode) -> Option<RowLayout> {
    match node.layout {
        Some(LayoutInfo::Row(row)) => Some(row),
        _ => None,
    }
}

// --- the rows of a box -------------------------------------------------------------------------

/// One row of a box, as it stands.
struct Site<'a> {
    node: &'a InspectionNode,
    grid: RowLayout,
    keys: Vec<String>,
}

#[derive(Default)]
struct Grid<'a> {
    /// Every row of one box, by the path of the box it stands in.
    boxes: BTreeMap<String, Vec<Site<'a>>>,
}

/// The rows of one box stand on one grid: their titles on one x for each depth, their content on
/// one right edge, and every nesting step indenting by the same gutter.
///
/// A section's header is not compared with them: a section may hold a panel of its own (a demo, a
/// reading), whose rows are inset by the panel, and the inspection cannot tell that list from the
/// section's own.
fn columns(root: &InspectionNode, out: &mut Vec<Problem>) {
    let mut found = Grid::default();
    collect_grid(root, &mut Vec::new(), &mut found);
    for rows in found.boxes.values() {
        check_box(rows, out);
    }
}

fn collect_grid<'a>(node: &'a InspectionNode, keys: &mut Vec<String>, found: &mut Grid<'a>) {
    let mut boxes: BTreeMap<String, Vec<Site<'a>>> = BTreeMap::new();
    for child in &node.children {
        let pushed = child.test_id.clone().map(|id| keys.push(id)).is_some();
        if let Some(grid) = grid(child) {
            boxes.entry(node.path.clone()).or_default().push(Site {
                node: child,
                grid,
                keys: keys.clone(),
            });
        }
        collect_grid(child, keys, found);
        if pushed {
            keys.pop();
        }
    }
    for (path, rows) in boxes {
        found.boxes.entry(path).or_default().extend(rows);
    }
}

fn check_box(rows: &[Site<'_>], out: &mut Vec<Problem>) {
    let Some(first) = rows.first() else {
        return;
    };
    let right = f64::from(first.grid.content_right);
    for row in rows.iter().skip(1) {
        if (f64::from(row.grid.content_right) - right).abs() > SLACK {
            push(
                out,
                Kind::RaggedColumns,
                row.node,
                &row.keys,
                format!(
                    "its content ends at x {:.0} where the rows of its box end at x {:.0}",
                    row.grid.content_right, right
                ),
            );
        }
    }
    // One title x for each depth: rows that stand as deep as each other stand on one line.
    let mut by_depth: BTreeMap<u32, f64> = BTreeMap::new();
    for row in rows {
        let x = f64::from(row.grid.text_x);
        match by_depth.get(&row.grid.depth) {
            Some(&expected) if (x - expected).abs() > SLACK => push(
                out,
                Kind::RaggedColumns,
                row.node,
                &row.keys,
                format!(
                    "its title starts at x {:.0} where the rows at depth {} start at x {:.0}: a \
                     row's leading control, its checkbox and its avatar stand in one column",
                    row.grid.text_x, row.grid.depth, expected
                ),
            ),
            Some(_) => {}
            None => {
                by_depth.insert(row.grid.depth, x);
            }
        }
    }
    // A nesting step indents by one gutter, whatever the depth.
    let Some((&base_depth, &base_x)) = by_depth.iter().next() else {
        return;
    };
    let next = by_depth.get(&base_depth.saturating_add(1)).copied();
    let Some(indent) = next.map(|x| x - base_x) else {
        return;
    };
    if indent.abs() <= SLACK {
        if let Some(row) = rows
            .iter()
            .find(|row| row.grid.depth == base_depth.saturating_add(1))
        {
            push(
                out,
                Kind::RaggedColumns,
                row.node,
                &row.keys,
                format!(
                    "a row {} deep starts at x {:.0}, as deep as one {} deep: nesting did not \
                     indent it",
                    row.grid.depth, row.grid.text_x, base_depth
                ),
            );
        }
        return;
    }
    for row in rows {
        let steps = row.grid.depth.saturating_sub(base_depth);
        let expected = base_x + f64::from(steps) * indent;
        if (f64::from(row.grid.text_x) - expected).abs() > SLACK {
            push(
                out,
                Kind::RaggedColumns,
                row.node,
                &row.keys,
                format!(
                    "its title starts at x {:.0}: {steps} nesting steps of {:.0} from x {:.0} put \
                     it at x {:.0}",
                    row.grid.text_x, indent, base_x, expected
                ),
            );
        }
    }
}

// --- a first level on its section's column ----------------------------------------------------

/// The title a box of rows stands under, as the nearest heading before it: where the title's
/// words start, and what they say.
#[derive(Clone, Debug)]
struct Heading {
    x: f64,
    words: String,
}

/// A list or tree whose first level stands more than one nesting step right of the title its
/// section goes by. A section's title starts on the column its content does, and a tree's first
/// level keeps the one step in front of it that its folding chevron hangs in; a second step on
/// top of that leaves the section's rows adrift from its own words, and a step kept by every
/// level leaves a deep row with no column left.
fn first_level(
    node: &InspectionNode,
    above: Option<&Heading>,
    step: f64,
    keys: &mut Vec<String>,
    out: &mut Vec<Problem>,
) -> Option<Heading> {
    if let Some(heading) = above {
        level_of_box(node, heading, step, keys.as_slice(), out);
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    // The heading a child stands under: the nearest one before it among its siblings, or the one
    // its box stood under.
    let mut running = above.cloned();
    let mut last = heading_of(node);
    for child in &node.children {
        if let Some(found) = first_level(child, running.as_ref(), step, keys, out) {
            running = Some(found.clone());
            last = Some(found);
        }
    }
    if pushed {
        keys.pop();
    }
    last
}

/// The heading `node` says itself with, where it is one.
fn heading_of(node: &InspectionNode) -> Option<Heading> {
    if !matches!(role(node), Some(Role::Heading { .. })) {
        return None;
    }
    Some(Heading {
        x: node.rect[0],
        words: semantics_name(node)
            .unwrap_or("its section's title")
            .to_owned(),
    })
}

/// One node's own rows, where it holds them: the first level on the column `heading` stands on,
/// kept at most one nesting `step` in from it.
fn level_of_box(
    node: &InspectionNode,
    heading: &Heading,
    step: f64,
    keys: &[String],
    out: &mut Vec<Problem>,
) {
    let rows: Vec<RowLayout> = node.children.iter().filter_map(grid).collect();
    let Some(base) = rows.iter().map(|row| row.depth).min() else {
        return;
    };
    // The title has to stand over the box's own column: a title in another column (a page's, a
    // sidebar's beside it) says nothing of where this box's rows start. One step to the left of
    // the box is still its column's, where a tree's folding chevron hangs.
    let boxed = rect(node);
    if heading.x < boxed.x0 - step - SLACK || heading.x > boxed.x1 + SLACK {
        return;
    }
    let Some(first) = rows
        .iter()
        .filter(|row| row.depth == base)
        .map(|row| f64::from(row.content_left))
        .fold(None, |least: Option<f64>, x| {
            Some(least.map_or(x, |y| y.min(x)))
        })
    else {
        return;
    };
    if first - heading.x <= step + SLACK {
        return;
    }
    push(
        out,
        Kind::FirstLevelIndent,
        node,
        keys,
        format!(
            "its first level starts at x {first:.0}, {:.0} px right of the words of \"{}\" at x \
             {:.0}: a list stands at most one nesting step of {step:.0} px in from its section's \
             column",
            first - heading.x,
            heading.words,
            heading.x
        ),
    );
}

// --- one choice, filled once -------------------------------------------------------------------

/// A choice is filled where the person chose it: a selected box inside another selected one draws
/// the same row twice lit.
fn selections(
    node: &InspectionNode,
    keys: &mut Vec<String>,
    selected_above: bool,
    out: &mut Vec<Problem>,
) {
    let selected = node
        .semantics
        .as_ref()
        .is_some_and(|semantics| semantics.state.selected);
    if selected && selected_above {
        push(
            out,
            Kind::DoubleSelection,
            node,
            keys,
            "its selected shape stands inside another selected box: one choice drawn twice"
                .to_owned(),
        );
    }
    if selected
        && let Some(semantics) = &node.semantics
        && semantics.children.iter().any(|child| child.state.selected)
    {
        push(
            out,
            Kind::DoubleSelection,
            node,
            keys,
            "a selected item inside its own selected list: one choice drawn twice".to_owned(),
        );
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    for child in &node.children {
        selections(child, keys, selected_above || selected, out);
    }
    if pushed {
        keys.pop();
    }
}

// --- the place the window shows stands chosen -------------------------------------------------

/// The shell of a window and the list of its places: what it is made of
/// ([`ui::widget::NavShell`], [`ui::widget::NavList`]).
const NAV_SHELL: &str = "NavShell";
const NAV_TITLE: &str = "NavShellTitle";
const NAV_LIST: &str = "NavList";

/// A window's header names what it shows, and the sidebar beside it offers the places: the place
/// the header names stands chosen in the sidebar, lit as the one the person reads.
fn chosen(node: &InspectionNode, shown: &[String], keys: &mut Vec<String>, out: &mut Vec<Problem>) {
    let titled = (node.widget == NAV_SHELL).then(|| titles(node));
    let shown = titled.as_deref().unwrap_or(shown);
    if node.widget == NAV_LIST
        && let Some(why) = unchosen(node, shown)
    {
        push(out, Kind::UnchosenPlace, node, keys, why);
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    for child in &node.children {
        chosen(child, shown, keys, out);
    }
    if pushed {
        keys.pop();
    }
}

/// What a window's headers call what they show: the title of its content, and its sidebar's.
fn titles(node: &InspectionNode) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.widget == NAV_TITLE
            && node
                .semantics
                .as_ref()
                .is_some_and(|semantics| matches!(semantics.role, Role::Heading { .. }))
            && let Some(name) = semantics_name(node)
        {
            out.push(name.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        pending.extend(node.children.iter());
    }
    out
}

/// What is wrong with `list` where the window shows one of its places and no place there stands
/// chosen: the place it offers, said for the person reading it.
fn unchosen(list: &InspectionNode, shown: &[String]) -> Option<String> {
    let places = places(list);
    if places.iter().any(|(_, chosen)| *chosen) {
        return None;
    }
    let (opened, _) = places
        .into_iter()
        .find(|(place, _)| shown.iter().any(|title| title == place))?;
    Some(format!(
        "the window shows \"{opened}\" and its sidebar offers it, yet no place there stands \
         chosen: the row of what the window shows is the one the person reads"
    ))
}

/// The places a navigation list offers, each as its name and whether it stands chosen.
fn places(list: &InspectionNode) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut pending = vec![list];
    while let Some(node) = pending.pop() {
        if let Some(semantics) = &node.semantics
            && matches!(semantics.role, Role::TreeItem)
            && let Some(name) = semantics_name(node)
        {
            let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push((name, semantics.state.selected));
        }
        pending.extend(node.children.iter());
    }
    out
}

// --- one way to add ----------------------------------------------------------------------------

/// A control that offers to add something: what it says, and whether it is a press or an entry to
/// type in.
struct Adder<'a> {
    node: &'a InspectionNode,
    label: String,
    /// The box it stands in holds rows as a list.
    in_list: bool,
    box_path: String,
    keys: Vec<String>,
}

/// One screen offers one way to add a thing: the same label twice on two controls, or two add
/// affordances in one list, makes a person choose between them.
fn adds(node: &InspectionNode, keys: &mut Vec<String>, in_row: bool, out: &mut Vec<Problem>) {
    let mut adders: Vec<Adder<'_>> = Vec::new();
    collect_adders(node, keys, in_row, node.path.as_str(), false, &mut adders);
    // Two add affordances in one list.
    let mut boxes: BTreeMap<&str, Vec<&Adder<'_>>> = BTreeMap::new();
    for adder in &adders {
        boxes
            .entry(adder.box_path.as_str())
            .or_default()
            .push(adder);
    }
    for (path, list) in &boxes {
        // The screen's own chrome may offer several actions (a project, a section): the rule is
        // about a list, which offers one way in, at its end.
        if list.len() < 2 || path.is_empty() || !list.first().is_some_and(|a| a.in_list) {
            continue;
        }
        let labels: Vec<String> = list
            .iter()
            .map(|adder| format!("\"{}\"", adder.label))
            .collect();
        let Some(second) = list.get(1) else {
            continue;
        };
        push(
            out,
            Kind::SeveralAdds,
            second.node,
            &second.keys,
            format!(
                "one list offers {} ways to add to it ({}): a list has one add entry, at its end",
                list.len(),
                labels.join(", ")
            ),
        );
    }
}

fn collect_adders<'a>(
    node: &'a InspectionNode,
    keys: &mut Vec<String>,
    in_row: bool,
    box_path: &str,
    in_list: bool,
    out: &mut Vec<Adder<'a>>,
) {
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    let role = role(node);
    // An entry field offers to add as a button does: a list's add entry is the field at its end.
    let control = matches!(
        role,
        Some(
            Role::TextInput
                | Role::MultilineTextInput
                | Role::Button
                | Role::ToggleButton
                | Role::MenuItem
                | Role::Link
        )
    );
    // What a row holds is the item's own content: a row's name is not the screen's offer to add.
    let in_row = in_row || matches!(role, Some(Role::ListItem | Role::TreeItem));
    if control
        && !in_row
        && let Some(name) = semantics_name(node)
        && starts_with_any(name, &ADD_VERBS)
    {
        out.push(Adder {
            node,
            label: name.to_owned(),
            in_list,
            box_path: box_path.to_owned(),
            keys: keys.clone(),
        });
    }
    for child in &node.children {
        let field = if node.widget == "Absent" {
            box_path
        } else {
            node.path.as_str()
        };
        collect_adders(child, keys, in_row, field, in_list || is_list(node), out);
    }
    if pushed {
        keys.pop();
    }
}

/// Whether the box holds rows as a list: what it says to a screen reader, or what it is.
pub(super) fn is_list(node: &InspectionNode) -> bool {
    matches!(role(node), Some(Role::List | Role::Tree | Role::Table))
        || node.widget.to_ascii_lowercase().ends_with("list")
        || node.widget.to_ascii_lowercase().ends_with("lane")
}

fn starts_with_any(name: &str, verbs: &[&str]) -> bool {
    let lower = name.to_lowercase();
    verbs.iter().any(|verb| lower.starts_with(verb))
}

// --- names a screen reader reads ---------------------------------------------------------------

/// A name a screen reader announces is words, not the code an identifier is written as: the
/// same rule the compiler holds a package's words to as it writes its catalogue
/// ([`tilolang::messages::reads_as_code`]).
fn names(node: &InspectionNode, keys: &mut Vec<String>, out: &mut Vec<Problem>) {
    if let Some(name) = semantics_name(node)
        && let Some(reason) = tilolang::messages::reads_as_code(name)
    {
        push(
            out,
            Kind::CodedName,
            node,
            keys,
            format!("\"{name}\" reads as a {reason} rather than as words"),
        );
    }
    if let Some(semantics) = &node.semantics {
        for child in &semantics.children {
            if let Some(name) = child.name.as_deref()
                && let Some(reason) = tilolang::messages::reads_as_code(name)
            {
                push(
                    out,
                    Kind::CodedName,
                    node,
                    keys,
                    format!("\"{name}\" reads as a {reason} rather than as words"),
                );
            }
        }
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    for child in &node.children {
        names(child, keys, out);
    }
    if pushed {
        keys.pop();
    }
}

// --- destructive verbs -------------------------------------------------------------------------

/// A control that destroys something says so with the one verb every screen uses for it. Only
/// what the view declared destroys is held to the verb: a button that clears a field's value
/// says "Remove" and destroys nothing.
fn verbs(
    node: &InspectionNode,
    declared: &Declared,
    keys: &mut Vec<String>,
    out: &mut Vec<Problem>,
) {
    if matches!(role(node), Some(Role::Button | Role::MenuItem))
        && node
            .test_id
            .as_deref()
            .is_some_and(|key| declared.destroys(key))
        && let Some(name) = semantics_name(node)
        && let Some(other) = other_delete_verb(name)
    {
        push(
            out,
            Kind::UnsharedVerb,
            node,
            keys,
            format!(
                "\"{name}\" destroys with \"{other}\" where every destructive control says \"{DELETE_VERB}\""
            ),
        );
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    for child in &node.children {
        verbs(child, declared, keys, out);
    }
    if pushed {
        keys.pop();
    }
}

/// The word `name` destroys with, where it is one of [`OTHER_DELETE_VERBS`]: read as a whole
/// word, so a control that says only "Discard" is held to the verb as one that says "Discard
/// view" is.
fn other_delete_verb(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    let first = lower
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default();
    OTHER_DELETE_VERBS
        .iter()
        .copied()
        .find(|verb| *verb == first)
}

// --- a surface inside a surface ------------------------------------------------------------------

/// A card in a card: a surface drawn on a surface raised above the page, twice over, is one
/// bound too many.
fn cards(node: &InspectionNode, keys: &mut Vec<String>, in_card: bool, out: &mut Vec<Problem>) {
    let card = node.widget == "Card";
    if card && in_card {
        push(
            out,
            Kind::NestedCard,
            node,
            keys,
            "a Card stands inside another Card: a surface holds its content on the page, not on \
             another surface"
                .to_owned(),
        );
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    for child in &node.children {
        cards(child, keys, in_card || card, out);
    }
    if pushed {
        keys.pop();
    }
}

// --- an item open where it stands ---------------------------------------------------------------

/// An item open where it stands is a row of the list the person sees: the row that opened it
/// stands in a list, within the columns the list's own box spans, and one list shows one item
/// open at a time. An item shown beside the list, or over the part of it on screen, is not the
/// item where it stands in the list they are reading. Its box may reach below the list's, or be
/// scrolled part way above its top: a document longer than the list is read by scrolling it.
fn opens(
    node: &InspectionNode,
    list: Option<&InspectionNode>,
    shown: &mut BTreeSet<String>,
    keys: &mut Vec<String>,
    out: &mut Vec<Problem>,
) {
    let list = if is_list(node) { Some(node) } else { list };
    // A list gives each row a node of its own in the tree a reader is handed, standing under the
    // list rather than as a widget of its own: that node is where the row's box is read. A
    // heading the list publishes says it is expanded too, and a tree's branch stays expanded
    // while its subtasks show, so only an item's own node is read, and only a list's row is
    // counted as an item standing open.
    if let Some(semantics) = &node.semantics {
        for item in &semantics.children {
            let is_item = matches!(item.role, Role::ListItem | Role::TreeItem);
            if !is_item || item.state.expanded != Some(true) {
                continue;
            }
            if let Some(row) = item.bounds.map(ui::a11y::AccessBounds::to_rect)
                && let Some(detail) = outside_list(list, row)
            {
                push(out, Kind::OpenOutsideList, node, keys, detail);
            }
            if item.role == Role::ListItem && second_open(list, shown) {
                push(
                    out,
                    Kind::OpenOutsideList,
                    node,
                    keys,
                    "a second row stands open in one list: a list opens one item at a time"
                        .to_owned(),
                );
            }
        }
    }
    // A row drawn as a widget of its own, outside a list's own nodes: the same rule, read from
    // the widget's box.
    let open = role(node) == Some(Role::ListItem)
        && node
            .semantics
            .as_ref()
            .is_some_and(|semantics| semantics.state.expanded == Some(true));
    if open {
        if let Some(detail) = outside_list(list, rect(node)) {
            push(out, Kind::OpenOutsideList, node, keys, detail);
        }
        if second_open(list, shown) {
            push(
                out,
                Kind::OpenOutsideList,
                node,
                keys,
                format!(
                    "a second {} stands open in one list: a list opens one item at a time",
                    node.widget
                ),
            );
        }
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    for child in &node.children {
        opens(child, list, shown, keys, out);
    }
    if pushed {
        keys.pop();
    }
}

/// What standing open says of an item whose box is `row`: the list it belongs to shows it where
/// it stands, or it stands outside the list the person is reading. A box reaching below the list
/// is fine — a document longer than the list is read by scrolling it — and so is one scrolled
/// part way above its top, which still reaches into it; a row lying wholly above the list stands
/// beside it, over the part of the list on screen.
fn outside_list(list: Option<&InspectionNode>, row: Rect) -> Option<String> {
    let Some(list) = list else {
        return Some(
            "the item stands open outside a list: a row opens where it stands in the list that \
             shows it, never beside it"
                .to_owned(),
        );
    };
    let boxed = rect(list);
    let outside =
        row.x0 < boxed.x0 - SLACK || row.x1 > boxed.x1 + SLACK || row.y1 <= boxed.y0 + SLACK;
    outside.then(|| {
        format!(
            "the row it opened stands outside the list it is in ({:.0}, {:.0})×({:.0}, {:.0})",
            row.x0, row.y0, row.x1, row.y1
        )
    })
}

/// Whether this is the second item standing open in `list`.
fn second_open(list: Option<&InspectionNode>, shown: &mut BTreeSet<String>) -> bool {
    !shown.insert(list.map_or_else(String::new, |list| list.path.clone()))
}

// --- a row's title, written once ----------------------------------------------------------------

/// A row that opened into its item is the item: its title stands once, in the field that edits it,
/// and what the item holds follows under it. The row's title left standing over the document says
/// the same words twice in one place.
///
/// The words counted are the ones a person reads ([`writes`]), never the name a box is announced
/// by: the item is its own row and is announced by its title, which is written once.
fn repeated_row_titles(node: &InspectionNode, keys: &mut Vec<String>, out: &mut Vec<Problem>) {
    if is_list(node) {
        let open: Vec<(String, Rect)> = node
            .semantics
            .as_ref()
            .map(|semantics| {
                semantics
                    .children
                    .iter()
                    .filter(|item| {
                        matches!(item.role, Role::ListItem | Role::TreeItem)
                            && item.state.expanded == Some(true)
                    })
                    .filter_map(|item| {
                        let name = item.name.as_deref()?;
                        let at = item.bounds.map(ui::a11y::AccessBounds::to_rect)?;
                        Some((one_line(name), at))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (title, at) in open {
            let shown = titled_within(node, &title, at);
            if shown > 1 {
                push(
                    out,
                    Kind::RepeatedRowTitle,
                    node,
                    keys,
                    format!(
                        "\"{title}\" stands {shown} times in the item its row opened into: the \
                         row became the item, its title written once"
                    ),
                );
                break;
            }
        }
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    for child in &node.children {
        repeated_row_titles(child, keys, out);
    }
    if pushed {
        keys.pop();
    }
}

/// How many widgets standing within `at` write `title` where a person reads it: a label, a
/// heading, the value of the field that edits it.
fn titled_within(node: &InspectionNode, title: &str, at: Rect) -> usize {
    let own = rect(node);
    let mut count = usize::from(
        at.contains(own.center())
            && node
                .semantics
                .as_ref()
                .is_some_and(|semantics| writes(semantics, title)),
    );
    for child in &node.children {
        count = count.saturating_add(titled_within(child, title, at));
    }
    count
}

/// Whether this node writes `title` on the screen: the words of a label, a heading or a link, or
/// the value of the field it is. A box's accessible name is not written anywhere: the row standing
/// open into its item is named by the title that item states (`Role::ListItem` and its like), and a
/// box that names what it holds says those words once, in what it holds.
fn writes(semantics: &ui::a11y::AccessNode, title: &str) -> bool {
    let drawn = matches!(
        semantics.role,
        Role::StaticText | Role::Label | Role::Heading { .. } | Role::Link
    );
    (drawn
        && semantics
            .name
            .as_deref()
            .is_some_and(|name| one_line(name) == title))
        || semantics
            .value
            .as_deref()
            .is_some_and(|value| one_line(value) == title)
}

/// `text` as one line of words: whitespace collapsed, ends trimmed.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- the gutter a narrow window keeps -----------------------------------------------------------

/// A window too narrow for a page's own gutters keeps one gutter at its edge, so content never
/// touches the glass.
fn gutter(node: &InspectionNode, width: f64, keys: &mut Vec<String>, out: &mut Vec<Problem>) {
    if width > COMPACT_WIDTH {
        return;
    }
    if let Some(grid) = grid(node) {
        // A row the window shows part of, or one inside a view that scrolls sideways, is placed by
        // what it holds, not by the window's edge.
        if f64::from(grid.content_left) < -SLACK
            || f64::from(grid.content_right) > width + SLACK
            || rect(node).x0 < -SLACK
        {
            return;
        }
        let content_left = f64::from(grid.content_left);
        if content_left < COMPACT_GUTTER - SLACK {
            push(
                out,
                Kind::CrampedGutter,
                node,
                keys,
                format!(
                    "its content starts {content_left:.0} px from the window's edge: a {width:.0} px \
                     window keeps {COMPACT_GUTTER:.0} px"
                ),
            );
        }
        let from_right = width - f64::from(grid.content_right);
        if from_right < COMPACT_GUTTER - SLACK {
            push(
                out,
                Kind::CrampedGutter,
                node,
                keys,
                format!(
                    "its content ends {from_right:.0} px from the window's edge: a {width:.0} px \
                     window keeps {COMPACT_GUTTER:.0} px"
                ),
            );
        }
    }
    let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
    for child in &node.children {
        gutter(child, width, keys, out);
    }
    if pushed {
        keys.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::AccessNode;

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
            paint_key: None,
            semantics: role.map(AccessNode::new),
            children: Vec::new(),
        }
    }

    fn named(widget: &str, id: &str, rect: [f64; 4], role: Role, name: &str) -> InspectionNode {
        let mut node = node(widget, id, rect, Some(role));
        node.semantics = Some(AccessNode::new(role).with_name(name));
        node
    }

    fn selected(widget: &str, id: &str, rect: [f64; 4], role: Role) -> InspectionNode {
        let mut node = node(widget, id, rect, Some(role));
        node.semantics =
            Some(AccessNode::new(role).with_state(ui::a11y::AccessState::new().selected(true)));
        node
    }

    /// A row's grid: its title at `text_x`, its content ending at `content_right`.
    fn grid(depth: u32, text_x: f64, content_right: f64) -> RowLayout {
        RowLayout {
            depth,
            content_left: f64_to_f32(text_x),
            text_x: f64_to_f32(text_x),
            text_right: f64_to_f32(content_right - 80.0),
            facts_left: f64_to_f32(content_right),
            content_right: f64_to_f32(content_right),
            marks_left: None,
        }
    }

    fn row(id: &str, depth: u32, text_x: f64, content_right: f64) -> InspectionNode {
        let mut row = named(
            "ListRow",
            id,
            [0.0, 0.0, content_right, 36.0],
            Role::ListItem,
            "A task",
        );
        row.layout = Some(LayoutInfo::Row(grid(depth, text_x, content_right)));
        row
    }

    fn holding(
        widget: &str,
        id: &str,
        role: Option<Role>,
        children: Vec<InspectionNode>,
    ) -> InspectionNode {
        let mut node = node(widget, id, [0.0, 0.0, 400.0, 300.0], role);
        node.children = children;
        node
    }

    fn f64_to_f32(value: f64) -> f32 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a row's columns in logical pixels"
        )]
        let narrow = value as f32;
        narrow
    }

    fn found(kind: Kind, root: &InspectionNode, viewport: (u32, u32)) -> Vec<Problem> {
        found_as(kind, root, viewport, &Declared::default())
    }

    /// What `root` holds wrong of `kind`, as `declared` says what its controls do.
    fn found_as(
        kind: Kind,
        root: &InspectionNode,
        viewport: (u32, u32),
        declared: &Declared,
    ) -> Vec<Problem> {
        inspect(root, viewport, declared, TOOLKIT_STEP)
            .into_iter()
            .filter(|problem| problem.kind == kind)
            .collect()
    }

    /// The declaration a view makes of the button `key`: pressing it destroys what it names.
    fn declared_destructive(key: &str) -> Declared {
        let mut button = UiNode::empty("ui.Button", key);
        button
            .props
            .insert("role".to_owned(), Value::text("destructive"));
        let mut declared = Declared::default();
        declared.read(&button);
        declared
    }

    /// Rows of one box stand their titles on one x at each depth, and every nesting step indents
    /// by the same gutter: a row whose leading control or nesting walks off that line is caught,
    /// and one that stands on it is not.
    #[test]
    fn rows_of_one_box_must_share_their_columns() {
        let shared = holding(
            "DescribedList",
            "list",
            Some(Role::List),
            vec![row("a", 0, 24.0, 400.0), row("b", 0, 24.0, 400.0)],
        );
        assert!(found(Kind::RaggedColumns, &shared, (1280, 800)).is_empty());

        let ragged = holding(
            "DescribedList",
            "list",
            Some(Role::List),
            vec![row("a", 0, 24.0, 400.0), row("b", 0, 40.0, 400.0)],
        );
        let problems = found(Kind::RaggedColumns, &ragged, (1280, 800));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].detail.contains("depth 0"), "{problems:?}");

        let indented = holding(
            "DescribedList",
            "list",
            Some(Role::List),
            vec![
                row("a", 0, 24.0, 400.0),
                row("b", 1, 44.0, 400.0),
                row("c", 2, 64.0, 400.0),
            ],
        );
        assert!(found(Kind::RaggedColumns, &indented, (1280, 800)).is_empty());

        let stepped = holding(
            "DescribedList",
            "list",
            Some(Role::List),
            vec![
                row("a", 0, 24.0, 400.0),
                row("b", 1, 44.0, 400.0),
                row("c", 2, 56.0, 400.0),
            ],
        );
        let problems = found(Kind::RaggedColumns, &stepped, (1280, 800));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].detail.contains("nesting steps"), "{problems:?}");
    }

    /// A list or tree keeps its first level within one nesting step of the title its section
    /// goes by: a step for the title's column and no second step for the first level.
    #[test]
    fn a_first_level_stands_within_one_step_of_its_sections_title() {
        let section = |first: f64, nested: f64| {
            holding(
                "Section",
                "section",
                None,
                vec![
                    named(
                        "Text",
                        "title",
                        [24.0, 0.0, 200.0, 20.0],
                        Role::Heading { level: 2 },
                        "Subtasks",
                    ),
                    holding(
                        "DescribedTreeView",
                        "tree",
                        Some(Role::Tree),
                        vec![row("a", 0, first, 400.0), row("b", 1, nested, 400.0)],
                    ),
                ],
            )
        };
        // The title's column, and the one step a folding chevron hangs in front of the rows.
        let standing = section(46.0, 68.0);
        assert!(found(Kind::FirstLevelIndent, &standing, (390, 844)).is_empty());
        // A plain list stands on the title's own column.
        let flush = section(24.0, 46.0);
        assert!(found(Kind::FirstLevelIndent, &flush, (390, 844)).is_empty());

        // A step for the chevron and another for the first level: the section's rows stand
        // adrift of the words that name them.
        let adrift = section(68.0, 90.0);
        let problems = found(Kind::FirstLevelIndent, &adrift, (390, 844));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].detail.contains("Subtasks"), "{problems:?}");
        assert_eq!(problems[0].widget, "DescribedTreeView", "{problems:?}");

        // A title beside the box, in another column of the page, says nothing of its rows.
        let mut beside = section(68.0, 90.0);
        beside.children[1].rect = [200.0, 0.0, 400.0, 300.0];
        assert!(found(Kind::FirstLevelIndent, &beside, (390, 844)).is_empty());
    }

    /// One choice is filled once: a selected row inside a selected box draws the same choice
    /// twice, where a selected row in a plain box draws it once.
    #[test]
    fn one_choice_is_filled_once() {
        let shape = || {
            holding(
                "Card",
                "card",
                None,
                vec![selected(
                    "ListRow",
                    "row",
                    [0.0, 0.0, 400.0, 36.0],
                    Role::ListItem,
                )],
            )
        };
        let mut once = shape();
        once.widget = "Stack".to_owned();
        assert!(found(Kind::DoubleSelection, &once, (1280, 800)).is_empty());

        // The box draws a shape of its own.
        let mut boxed = shape();
        boxed.semantics = Some(
            AccessNode::new(Role::List).with_state(ui::a11y::AccessState::new().selected(true)),
        );
        let problems = found(Kind::DoubleSelection, &boxed, (1280, 800));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].widget, "ListRow");
    }

    /// A place in a list is lit while it stands chosen.
    fn lit(widget: &str, id: &str, rect: [f64; 4], name: &str) -> InspectionNode {
        let mut node = named(widget, id, rect, Role::TreeItem, name);
        node.semantics = Some(
            AccessNode::new(Role::TreeItem)
                .with_name(name)
                .with_state(ui::a11y::AccessState::new().selected(true)),
        );
        node
    }

    /// The place a window shows is lit in its sidebar: the header naming a place the sidebar
    /// offers while no place there stands chosen leaves the person unable to see where they are,
    /// and a chosen place says it.
    #[test]
    fn the_place_a_window_shows_stands_chosen() {
        let shell = |chosen: bool| {
            let design = if chosen {
                lit("NavRow", "design", [0.0, 64.0, 200.0, 28.0], "design")
            } else {
                named(
                    "NavRow",
                    "design",
                    [0.0, 64.0, 200.0, 28.0],
                    Role::TreeItem,
                    "design",
                )
            };
            holding(
                "NavShell",
                "shell",
                None,
                vec![
                    holding(
                        "NavContent",
                        "content",
                        None,
                        vec![named(
                            "NavShellTitle",
                            "title",
                            [260.0, 0.0, 200.0, 24.0],
                            Role::Heading { level: 1 },
                            "design",
                        )],
                    ),
                    holding(
                        "NavList",
                        "places",
                        Some(Role::Tree),
                        vec![
                            named(
                                "NavRow",
                                "general",
                                [0.0, 36.0, 200.0, 28.0],
                                Role::TreeItem,
                                "general",
                            ),
                            design,
                        ],
                    ),
                    named(
                        "NavShellTitle",
                        "sidebar-title",
                        [8.0, 0.0, 120.0, 24.0],
                        Role::Heading { level: 2 },
                        "Channels",
                    ),
                ],
            )
        };
        let problems = found(Kind::UnchosenPlace, &shell(false), (1280, 800));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].detail.contains("design"), "{problems:?}");
        assert!(
            found(Kind::UnchosenPlace, &shell(true), (1280, 800)).is_empty(),
            "a chosen place says where the person is"
        );
    }

    /// A window too narrow for a page's gutters keeps one at its edge: content eight pixels from
    /// the glass is caught, content a gutter in is not.
    #[test]
    fn a_narrow_window_keeps_a_gutter() {
        let narrow = |text_x: f64, content_right: f64| {
            holding(
                "DescribedList",
                "list",
                Some(Role::List),
                vec![row("a", 0, text_x, content_right)],
            )
        };
        assert!(found(Kind::CrampedGutter, &narrow(20.0, 370.0), (390, 844)).is_empty());
        let problems = found(Kind::CrampedGutter, &narrow(8.0, 390.0), (390, 844));
        assert_eq!(problems.len(), 2, "{problems:?}");
        // A window with room for its own page keeps its content where the page puts it.
        assert!(found(Kind::CrampedGutter, &narrow(8.0, 390.0), (1280, 800)).is_empty());
    }

    /// One screen offers one way to add: two add affordances in one list, or one label on two
    /// controls, makes a person choose between them.
    #[test]
    fn a_screen_offers_one_way_to_add() {
        let single = holding(
            "Page",
            "page",
            None,
            vec![named(
                "TextInput",
                "entry",
                [0.0, 0.0, 300.0, 32.0],
                Role::TextInput,
                "New task",
            )],
        );
        assert!(found(Kind::SeveralAdds, &single, (1280, 800)).is_empty());

        let twice_in_one_list = holding(
            "DescribedList",
            "list",
            Some(Role::List),
            vec![
                named(
                    "TextInput",
                    "entry",
                    [0.0, 0.0, 300.0, 32.0],
                    Role::TextInput,
                    "New task",
                ),
                named(
                    "Button",
                    "add",
                    [0.0, 40.0, 100.0, 32.0],
                    Role::Button,
                    "Add task",
                ),
            ],
        );
        let problems = found(Kind::SeveralAdds, &twice_in_one_list, (1280, 800));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].detail.contains("one list"), "{problems:?}");

        // The screen's chrome offers the actions of the screen, one per thing: a header with two
        // of them is not a list.
        let chrome = holding(
            "NavShell",
            "shell",
            None,
            vec![
                named(
                    "Button",
                    "new-task",
                    [0.0, 0.0, 100.0, 32.0],
                    Role::Button,
                    "New task",
                ),
                named(
                    "Button",
                    "new-project",
                    [110.0, 0.0, 100.0, 32.0],
                    Role::Button,
                    "New project",
                ),
            ],
        );
        assert!(found(Kind::SeveralAdds, &chrome, (1280, 800)).is_empty());

        // One add entry, at the end of each list: rows of a board each offer their own.
        let lanes = holding(
            "Board",
            "board",
            None,
            (0..3)
                .map(|index| {
                    holding(
                        "BoardLane",
                        &format!("lane{index}"),
                        Some(Role::List),
                        vec![named(
                            "Button",
                            "add",
                            [0.0, 0.0, 100.0, 32.0],
                            Role::Button,
                            "New card",
                        )],
                    )
                })
                .collect(),
        );
        assert!(found(Kind::SeveralAdds, &lanes, (1280, 800)).is_empty());

        // A row's own name is content, not the screen's offer to add.
        let mut row_called_new = named(
            "ListRow",
            "row",
            [0.0, 0.0, 300.0, 36.0],
            Role::ListItem,
            "New year",
        );
        row_called_new.children = vec![];
        let content = holding(
            "DescribedList",
            "list",
            Some(Role::List),
            vec![row_called_new],
        );
        assert!(found(Kind::SeveralAdds, &content, (1280, 800)).is_empty());
    }

    /// A name is words a person says, not the identifier the code keeps it under.
    #[test]
    fn a_name_is_not_a_code_identifier() {
        let say = |name: &str| {
            holding(
                "Page",
                "page",
                None,
                vec![named(
                    "Button",
                    "button",
                    [0.0, 0.0, 100.0, 32.0],
                    Role::Button,
                    name,
                )],
            )
        };
        assert!(found(Kind::CodedName, &say("Save draft"), (1280, 800)).is_empty());
        // A brand wears the same shape (macOS), so capital letters inside a word say nothing on
        // their own: what is caught is the punctuation an identifier is written with.
        for code in ["save_draft", "notes.save()", "store::notes"] {
            let problems = found(Kind::CodedName, &say(code), (1280, 800));
            assert_eq!(problems.len(), 1, "{code}: {problems:?}");
        }
        // A name built from a view and a binding names the binding, which a screen reader reads
        // out as one letter; a one-letter name of its own is a letter ("X" on an axis).
        for code in ["Note editor for t", "Note editor for c"] {
            let problems = found(Kind::CodedName, &say(code), (1280, 800));
            assert_eq!(problems.len(), 1, "{code}: {problems:?}");
            assert!(problems[0].detail.contains("code"), "{problems:?}");
        }
        assert!(found(Kind::CodedName, &say("X"), (1280, 800)).is_empty());
        assert!(found(Kind::CodedName, &say("A note"), (1280, 800)).is_empty());
        assert!(found(Kind::CodedName, &say("I pinned this"), (1280, 800)).is_empty());
    }

    /// A control that destroys something says "Delete", as every screen does; one that clears a
    /// field's value declares no destruction and may say "Remove".
    #[test]
    fn a_destructive_control_says_delete() {
        let say = |name: &str| {
            holding(
                "Page",
                "page",
                None,
                vec![named(
                    "Button",
                    "button",
                    [0.0, 0.0, 100.0, 32.0],
                    Role::Button,
                    name,
                )],
            )
        };
        let destroys = declared_destructive("button");
        assert!(
            found_as(
                Kind::UnsharedVerb,
                &say("Delete project"),
                (1280, 800),
                &destroys
            )
            .is_empty()
        );
        let problems = found_as(
            Kind::UnsharedVerb,
            &say("Remove project"),
            (1280, 800),
            &destroys,
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].detail.contains("Delete"), "{problems:?}");

        // One word is the whole label: a control that says only "Discard" destroys with as
        // plain a verb as one that says "Discard view".
        for bare in [
            "Remove",
            "Discard",
            "Erase",
            "Purge",
            "Wipe",
            "Drop",
            "discard…",
            "Clear",
            "Clear done",
            "clear…",
        ] {
            let problems = found_as(Kind::UnsharedVerb, &say(bare), (1280, 800), &destroys);
            assert_eq!(problems.len(), 1, "{bare}: {problems:?}");
        }
        // A control that deletes the records it names says "Delete", whatever the noun it takes
        // away is called.
        let problems = found_as(
            Kind::UnsharedVerb,
            &say("Delete finished tasks"),
            (1280, 800),
            &destroys,
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert!(
            found_as(
                Kind::UnsharedVerb,
                &say("Discarded"),
                (1280, 800),
                &destroys
            )
            .is_empty(),
            "a word that only starts with a verb does not destroy with it"
        );

        // A field's own button clears the value the field holds: the view declares nothing of
        // it, so the word it says is its own.
        for cleared in ["Remove time", "Clear time"] {
            assert!(
                found(Kind::UnsharedVerb, &say(cleared), (1280, 800)).is_empty(),
                "a control the view declares nothing of is nobody's destructive control: {cleared}"
            );
        }
    }

    /// A surface holds its content on the page: a card drawn on a card is one surface too many.
    #[test]
    fn a_card_is_not_drawn_on_a_card() {
        let card = |inner: Vec<InspectionNode>| {
            holding(
                "Card",
                "card",
                None,
                vec![holding("Stack", "stack", None, inner)],
            )
        };
        assert!(found(Kind::NestedCard, &card(vec![]), (1280, 800)).is_empty());
        let problems = found(Kind::NestedCard, &card(vec![card(vec![])]), (1280, 800));
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    /// An item standing open where it stands: the row of the list, below its top edge.
    fn opened(id: &str, rect: [f64; 4], role: Role) -> InspectionNode {
        let mut row = node("ListItem", id, rect, Some(role));
        row.semantics =
            Some(AccessNode::new(role).with_state(ui::a11y::AccessState::new().expanded(true)));
        row
    }

    fn list_of(rows: Vec<InspectionNode>) -> InspectionNode {
        let mut list = holding("VirtualList", "list", Some(Role::List), rows);
        list.rect = [0.0, 100.0, 600.0, 700.0];
        list
    }

    #[test]
    fn an_open_item_is_a_row_of_the_list_the_person_sees() {
        let inside = list_of(vec![opened(
            "row",
            [16.0, 120.0, 584.0, 420.0],
            Role::ListItem,
        )]);
        assert!(found(Kind::OpenOutsideList, &inside, (1280, 800)).is_empty());

        // A document longer than the list is read by scrolling: its row may reach below it.
        let long = list_of(vec![opened(
            "row",
            [16.0, 120.0, 584.0, 2400.0],
            Role::ListItem,
        )]);
        assert!(found(Kind::OpenOutsideList, &long, (1280, 800)).is_empty());

        // An item standing open beside the list is not the item where it stands in it.
        let beside = holding(
            "Stack",
            "window",
            None,
            vec![
                list_of(vec![named(
                    "ListRow",
                    "row",
                    [16.0, 120.0, 584.0, 156.0],
                    Role::ListItem,
                    "A task",
                )]),
                opened("page", [620.0, 120.0, 1200.0, 420.0], Role::ListItem),
            ],
        );
        assert_eq!(
            found(Kind::OpenOutsideList, &beside, (1280, 800)).len(),
            1,
            "an item shown beside its list"
        );

        // An item open with no list at all, and two open rows of one list.
        let alone = opened("page", [0.0, 0.0, 600.0, 300.0], Role::ListItem);
        assert_eq!(found(Kind::OpenOutsideList, &alone, (1280, 800)).len(), 1);
        let twice = list_of(vec![
            opened("one", [16.0, 120.0, 584.0, 420.0], Role::ListItem),
            opened("two", [16.0, 430.0, 584.0, 700.0], Role::ListItem),
        ]);
        assert_eq!(found(Kind::OpenOutsideList, &twice, (1280, 800)).len(), 1);

        // A tree's branches open and fold their items, which is not an item showing where it
        // stands.
        let folded = list_of(vec![opened(
            "branch",
            [16.0, 120.0, 584.0, 156.0],
            Role::TreeItem,
        )]);
        assert!(found(Kind::OpenOutsideList, &folded, (1280, 800)).is_empty());
    }

    /// A list's rows are the nodes the list publishes under its own, not widgets of their own: a
    /// row open where it stands is read from the box its node reports.
    #[test]
    fn a_row_a_list_publishes_says_where_it_stands_in_it() {
        fn published(rows: Vec<ui::a11y::AccessNode>) -> InspectionNode {
            let mut list = list_of(vec![]);
            list.semantics = Some(AccessNode::new(Role::List).with_children(rows));
            list
        }
        fn row(role: Role, rect: [f64; 4]) -> ui::a11y::AccessNode {
            AccessNode::new(role)
                .with_state(ui::a11y::AccessState::new().expanded(true))
                .with_bounds(Rect::new(
                    rect[0],
                    rect[1],
                    rect[0] + rect[2],
                    rect[1] + rect[3],
                ))
        }

        let inside = published(vec![row(Role::ListItem, [16.0, 120.0, 584.0, 2400.0])]);
        assert!(found(Kind::OpenOutsideList, &inside, (1280, 800)).is_empty());

        // Scrolled part way out of the list's top, the row still reaches into it.
        let scrolled = published(vec![row(Role::ListItem, [16.0, 60.0, 584.0, 300.0])]);
        assert!(found(Kind::OpenOutsideList, &scrolled, (1280, 800)).is_empty());

        // Lying wholly above the list's top, the row stands beside what the person reads.
        let above = published(vec![row(Role::ListItem, [16.0, 20.0, 584.0, 60.0])]);
        assert_eq!(
            found(Kind::OpenOutsideList, &above, (1280, 800)).len(),
            1,
            "an item the list publishes over the top of its own box"
        );

        let beside = published(vec![row(Role::ListItem, [620.0, 120.0, 580.0, 300.0])]);
        assert_eq!(
            found(Kind::OpenOutsideList, &beside, (1280, 800)).len(),
            1,
            "an item the list publishes beside its own box"
        );

        let twice = published(vec![
            row(Role::ListItem, [16.0, 120.0, 584.0, 300.0]),
            row(Role::ListItem, [16.0, 430.0, 584.0, 270.0]),
        ]);
        assert_eq!(
            found(Kind::OpenOutsideList, &twice, (1280, 800)).len(),
            1,
            "two rows of one list standing open"
        );

        // A tree's branch says it is expanded while its subtasks show, which is not an item
        // standing open where it stands.
        let branch = published(vec![
            row(Role::TreeItem, [16.0, 120.0, 584.0, 36.0]),
            row(Role::TreeItem, [16.0, 160.0, 584.0, 36.0]),
        ]);
        assert!(found(Kind::OpenOutsideList, &branch, (1280, 800)).is_empty());
    }

    /// A row that opened into its item writes its title once: in the field that edits it, not
    /// again over the document the row became.
    #[test]
    fn an_open_item_writes_its_title_once() {
        fn field() -> InspectionNode {
            let at = [16.0, 130.0, 300.0, 32.0];
            let mut field = node("TextInput", "title", at, Some(Role::MultilineTextInput));
            field.semantics = Some(
                AccessNode::new(Role::MultilineTextInput)
                    .with_name("Title")
                    .with_value("Call the landlord about the boiler"),
            );
            field
        }
        fn list(title_nodes: Vec<InspectionNode>) -> InspectionNode {
            let mut list = list_of(title_nodes);
            list.semantics = Some(AccessNode::new(Role::List).with_children(vec![
                AccessNode::new(Role::ListItem)
                    .with_name("Call the landlord about the boiler")
                    .with_state(ui::a11y::AccessState::new().expanded(true))
                    .with_bounds(Rect::new(0.0, 100.0, 400.0, 500.0)),
            ]));
            list
        }

        assert!(
            found(Kind::RepeatedRowTitle, &list(vec![field()]), (1280, 800)).is_empty(),
            "the item's own title, in the field that edits it"
        );

        // The item is its own row: the box the row opened into is announced by the title the
        // document states, and says nothing of its own on screen.
        let mut row = holding("RowOpen", "row", Some(Role::ListItem), vec![field()]);
        row.rect = [0.0, 100.0, 400.0, 400.0];
        row.semantics = Some(
            AccessNode::new(Role::ListItem)
                .with_name("Call the landlord about the boiler")
                .with_state(ui::a11y::AccessState::new().expanded(true)),
        );
        assert!(
            found(Kind::RepeatedRowTitle, &list(vec![row]), (1280, 800)).is_empty(),
            "the row's name is the item's title, written once in the field that edits it"
        );

        // The row's label left standing over the document: the same words twice.
        let label = named(
            "Text",
            "label",
            [16.0, 100.0, 300.0, 36.0],
            Role::StaticText,
            "Call the landlord about the boiler",
        );
        let problems = found(
            Kind::RepeatedRowTitle,
            &list(vec![label, field()]),
            (1280, 800),
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].detail.contains("stands 2 times"),
            "{problems:?}"
        );
    }
}
