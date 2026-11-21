//! A list as elements: each row is named by the row it is, a tree's notes nest in a `<ul>` inside
//! the row's own `<li>`, under the row's own line, and a branch row carries the disclosure its
//! painted chevron stands for. A page therefore shows the same rows the window paints, presses a
//! row the list's own way rather than where the frame put it, folds a branch with the same
//! action, and reads the rows at the levels the list gives them.

use std::iter::Peekable;

use vello::kurbo::Rect;

use super::{ListKey, VirtualRows};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::composition::{Composed, Own};
use crate::i18n::widget_text;
use crate::icons::Icon;
use crate::theme::Theme;

impl VirtualRows {
    /// Name each composed row by the row it is, so a page presses the row's own action (`key:`)
    /// rather than a point in the frame's layout: a page shows every row the list holds, where the
    /// frame a row composed in may end before it, and a press at a row past that end stands outside
    /// the list and lands on nothing.
    pub(super) fn name_page_rows(&self, rows: &mut [Composed]) {
        let Some(encode) = &self.semantic_key else {
            return;
        };
        // A composed row is paired with the row the list holds as a tree's rows are
        // ([`Self::tree_html_rows`]): by the order the list composes them in when the two are the
        // same set, else by the box each stands at, looked for from the one before it as both are
        // in the list's order.
        let by_order = rows.len() == self.mounted.len();
        let mut from = 0;
        for (at, row) in rows.iter_mut().enumerate() {
            let mounted = if by_order {
                self.mounted.get(at)
            } else {
                self.mounted[from..]
                    .iter()
                    .position(|mounted| same_box(mounted.widget.get_rect(), row))
                    .and_then(|offset| {
                        from = from.saturating_add(offset);
                        self.mounted.get(from)
                    })
            };
            let Some(mounted) = mounted else {
                continue;
            };
            if let Some(semantics) = &mut row.semantics
                && semantics.action_target.is_none()
            {
                semantics.action_target = Some(format!("key:{}", encode(&mounted.key)));
            }
        }
    }

    /// The list's rows as elements: a flat list where the list is not a tree, and otherwise
    /// each row's notes nested in a `<ul>` inside the row's own element, under the row's own
    /// line, with a branch row's disclosure where its chevron does.
    pub(crate) fn tree_html_rows(&self, rows: Vec<Composed>, theme: &Theme) -> Vec<Composed> {
        let Some(marks) = self.tree.as_ref() else {
            return rows;
        };
        // Pair each composed row with the row the list holds. The composition lays out the rows
        // the list holds, in the order it holds them, so that order is what names them when the
        // two are the same set; a row whose box drifted from the one the list laid out (a page
        // shapes a row taller than the list gives it) is otherwise found by where it stands.
        let by_order = rows.len() == self.mounted.len();
        let mut paired: Vec<(u32, ListKey, Composed)> = Vec::with_capacity(rows.len());
        let mut loose: Vec<Composed> = Vec::new();
        for (at, row) in rows.into_iter().enumerate() {
            let mounted = if by_order {
                self.mounted.get(at)
            } else {
                self.mounted.iter().find(|mounted| {
                    let rect = mounted.widget.get_rect();
                    near(rect.x0, row.rect[0])
                        && near(rect.y0, row.rect[1])
                        && near(rect.width(), row.rect[2])
                        && near(rect.height(), row.rect[3])
                })
            };
            match mounted {
                Some(mounted) => paired.push((marks.depth(&mounted.key), mounted.key.clone(), row)),
                None => loose.push(row),
            }
        }
        for (_, key, row) in &mut paired {
            let Some(rect) = self.disclosure_rect(key) else {
                continue;
            };
            let folded = marks.is_folded(key);
            let part = self.disclosure_html(key, &row.path, rect, folded, theme);
            row.children.insert(0, part);
        }
        let mut rows = paired
            .into_iter()
            .map(|(depth, key, row)| Nesting {
                depth,
                holds: self.row_arrived(&key),
                row,
            })
            .peekable();
        let base = rows.peek().map_or(0, |row| row.depth);
        let mut nested = nest_rows(&mut rows, base);
        // A row the mounted window does not reach stands as it composed, after the tree.
        nested.extend(rows.map(|nesting| nesting.row));
        nested.extend(loose);
        nested
    }

    /// A branch row's disclosure as an element: a button named for what it does, where
    /// the chevron is painted, pressing the list's own fold on the row it belongs to.
    fn disclosure_html(
        &self,
        key: &ListKey,
        row_path: &str,
        rect: Rect,
        folded: bool,
        theme: &Theme,
    ) -> Composed {
        let name = widget_text(if folded {
            "tree-branch-expand"
        } else {
            "tree-branch-collapse"
        });
        let icon = if folded {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        };
        let mut part = Composed::new(
            disclosure_path(row_path),
            "RowDisclosure",
            [rect.x0, rect.y0, rect.width(), rect.height()],
        );
        part.tag = Some("button");
        part.focusable = true;
        // The chevron alone, centred where it is painted, at the size of a small icon.
        let side = f64::from(theme.tokens.metrics.icon_sm).min(rect.width().min(rect.height()));
        part.own = crate::composition::glyph(icon, side, theme.text_dim, false).map(|glyph| {
            Own::Face(crate::composition::Face {
                radius: f64::from(theme.tokens.radii.control),
                ..crate::composition::Face::glyph(glyph)
            })
        });
        let mut node = AccessNode::new(Role::Button)
            .with_name(name)
            .with_state(AccessState::new().expanded(!folded));
        if let Some(encode) = &self.semantic_key {
            node = node.with_action_target(format!("fold:{}", encode(key)));
        }
        part.semantics = Some(node);
        part
    }
}

/// A composed row at the level the tree gives it, and whether it holds the rows under it: a row
/// still on its way is a placeholder, which holds nothing until it arrives.
struct Nesting {
    depth: u32,
    holds: bool,
    row: Composed,
}

/// The rows as elements, each row's notes nested in a `<ul>` of their own inside it: `depth` is
/// the level the rows at this call stand at. The notes of a row that holds nothing yet stand
/// after it, at its level.
fn nest_rows(rows: &mut Peekable<impl Iterator<Item = Nesting>>, depth: u32) -> Vec<Composed> {
    let mut out = Vec::new();
    while rows.peek().is_some_and(|next| next.depth == depth) {
        let Some(Nesting { holds, row, .. }) = rows.next() else {
            break;
        };
        let deeper = rows.peek().map(|next| next.depth).filter(|at| *at > depth);
        match deeper {
            Some(deeper) if holds => {
                let children = nest_rows(rows, deeper);
                let notes = group(&row.path, row.rect, children);
                out.push(holding(row, notes));
            }
            Some(deeper) => {
                out.push(row);
                out.extend(nest_rows(rows, deeper));
            }
            None => out.push(row),
        }
    }
    out
}

/// `row` holding `notes` under it, as a page lays them out: the row's element is a column as
/// tall as both, its own line first, laid out and grounded as the row composed itself, and the
/// notes below it, as far in as the rows under it stand.
fn holding(mut row: Composed, mut notes: Composed) -> Composed {
    let mut line = Composed::new(line_path(&row.path), "TreeRowLine", row.rect);
    line.tag = Some("div");
    line.layout = row.layout.take();
    line.flows = row.flows;
    line.ground = row.ground.take();
    line.own = row.own.take();
    line.children = std::mem::take(&mut row.children);
    let [x, y, width, height] = row.rect;
    let [nx, ny, nwidth, nheight] = notes.rect;
    notes.layout = Some(format!(
        "margin:{}px 0 0 {}px;",
        (ny - (y + height)).max(0.0),
        (nx - x).max(0.0)
    ));
    row.rect = [
        x,
        y,
        (x + width).max(nx + nwidth) - x,
        (y + height).max(ny + nheight) - y,
    ];
    row.layout = Some("display:flex;flex-direction:column;min-width:0;".to_owned());
    row.flows = true;
    row.children = vec![line, notes];
    row
}

/// `children` in a group of their own under `parent`, over the region they take.
fn group(parent: &str, row: [f64; 4], children: Vec<Composed>) -> Composed {
    let mut rect = Rect::new(
        children.first().map_or(row[0], |child| child.rect[0]),
        children.first().map_or(row[1], |child| child.rect[1]),
        children.first().map_or(row[0], |child| child.rect[0]),
        children.first().map_or(row[1], |child| child.rect[1]),
    );
    for child in &children {
        rect = rect.union(Rect::new(
            child.rect[0],
            child.rect[1],
            child.rect[0] + child.rect[2],
            child.rect[1] + child.rect[3],
        ));
    }
    let mut group = Composed::new(
        notes_path(parent),
        "TreeNotes",
        [rect.x0, rect.y0, rect.width(), rect.height()],
    );
    group.tag = Some("ul");
    group.children = children;
    group
}

/// Where the tree composes the disclosure of the row at `row`.
fn disclosure_path(row: &str) -> String {
    format!("{row}/disclosure")
}

/// Where the tree composes the own line of a row at `row` that holds others, apart from them.
fn line_path(row: &str) -> String {
    format!("{row}/line")
}

/// Where the tree composes the notes of the row at `row`, the rows under it.
fn notes_path(row: &str) -> String {
    format!("{row}/notes")
}

fn near(left: f64, right: f64) -> bool {
    (left - right).abs() < 0.5
}

/// Whether `rect` is the box the `row` element was composed at.
fn same_box(rect: Rect, row: &Composed) -> bool {
    near(rect.x0, row.rect[0])
        && near(rect.y0, row.rect[1])
        && near(rect.width(), row.rect[2])
        && near(rect.height(), row.rect[3])
}
