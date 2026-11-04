//! What a person sees wrong on a laid-out screen that its geometry alone calls fine: a list whose
//! rows stand on different columns, one choice filled twice, the same add affordance standing
//! several times over, a name a screen reader reads as code, a destructive verb of its own, a card
//! inside a card.
//!
//! Every rule is read off the inspection alone — the retained boxes, their clips, the grid each row
//! reports and the semantics a screen reader is given — so no app has to know about it, and one
//! that composes its screen differently is held to the same rules as every other.

use std::collections::BTreeMap;

use ui::a11y::Role;
use ui::headless::InspectionNode;
use ui::kurbo::Rect;
use ui::widget::{LayoutInfo, RowLayout};

use super::audit::{Kind, Problem};

/// How far out of line two edges may stand, in logical pixels: layout places them on fractions of
/// a pixel, which a person does not see.
const SLACK: f64 = 1.0;

/// The room a screen keeps between its content and the window's edge on the narrowest window it is
/// made for.
pub const COMPACT_GUTTER: f64 = 16.0;

/// The window width under which a screen has no room for a page's own gutters and must keep the one
/// gutter itself.
const COMPACT_WIDTH: f64 = 600.0;

/// The words a screen's copy starts an add control with.
const ADD_VERBS: [&str; 3] = ["add ", "new ", "create "];

/// The words a screen's copy starts a destructive control with, other than the one verb every
/// screen destroys with.
const OTHER_DELETE_VERBS: [&str; 6] = ["remove ", "erase ", "discard ", "purge ", "wipe ", "drop "];

/// The one verb a destructive control says its action with.
pub const DELETE_VERB: &str = "Delete";

/// What this screen holds that a person would see wrong though its geometry is sound.
pub fn inspect(root: &InspectionNode, viewport: (u32, u32)) -> Vec<Problem> {
    let mut out = Vec::new();
    columns(root, &mut out);
    selections(root, &mut Vec::new(), false, &mut out);
    adds(root, &mut Vec::new(), false, &mut out);
    names(root, &mut Vec::new(), &mut out);
    verbs(root, &mut Vec::new(), &mut out);
    cards(root, &mut Vec::new(), false, &mut out);
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
fn is_list(node: &InspectionNode) -> bool {
    matches!(role(node), Some(Role::List | Role::Tree | Role::Table))
        || node.widget.to_ascii_lowercase().ends_with("list")
        || node.widget.to_ascii_lowercase().ends_with("lane")
}

fn starts_with_any(name: &str, verbs: &[&str]) -> bool {
    let lower = name.to_lowercase();
    verbs.iter().any(|verb| lower.starts_with(verb))
}

// --- names a screen reader reads ---------------------------------------------------------------

/// A name a screen reader announces is words, not the code an identifier is written as.
fn names(node: &InspectionNode, keys: &mut Vec<String>, out: &mut Vec<Problem>) {
    if let Some(name) = semantics_name(node)
        && let Some(reason) = identifier_shape(name)
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
                && let Some(reason) = identifier_shape(name)
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

/// Why `name` reads as code, if it does: the shape of an identifier rather than of the words a
/// person says.
fn identifier_shape(name: &str) -> Option<&'static str> {
    if name.contains('_') && !name.contains(' ') {
        return Some("code identifier");
    }
    if name.contains("::") {
        return Some("code path");
    }
    for word in name.split(|c: char| c.is_whitespace() || c == ',' || c == '.') {
        if word.is_empty() {
            continue;
        }
        if word.ends_with("()") {
            return Some("function call");
        }
    }
    None
}

// --- destructive verbs -------------------------------------------------------------------------

/// A control that destroys something says so with the one verb every screen uses for it.
fn verbs(node: &InspectionNode, keys: &mut Vec<String>, out: &mut Vec<Problem>) {
    if matches!(role(node), Some(Role::Button | Role::MenuItem))
        && let Some(name) = semantics_name(node)
        && let Some(other) = OTHER_DELETE_VERBS
            .iter()
            .find(|verb| name.to_lowercase().starts_with(**verb))
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
        verbs(child, keys, out);
    }
    if pushed {
        keys.pop();
    }
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
            layout: None,
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
        inspect(root, viewport)
            .into_iter()
            .filter(|problem| problem.kind == kind)
            .collect()
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
    }

    /// A control that destroys something says "Delete", as every screen does.
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
        assert!(found(Kind::UnsharedVerb, &say("Delete project"), (1280, 800)).is_empty());
        let problems = found(Kind::UnsharedVerb, &say("Remove project"), (1280, 800));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].detail.contains("Delete"), "{problems:?}");
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
}
