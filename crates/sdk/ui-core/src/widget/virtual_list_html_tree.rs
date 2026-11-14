//! A tree list as elements: each row's notes nest in a `<ul>` inside the row's own `<li>`, and
//! a branch row carries the disclosure its painted chevron stands for. A page therefore shows
//! the same tree the window paints, folds a branch with the same action, and reads the rows at
//! the levels the list gives them.

use std::iter::Peekable;

use vello::kurbo::Rect;

use super::{ListModel, VirtualList};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::composition::{Composed, Own};
use crate::i18n::widget_text;
use crate::icons::Icon;
use crate::theme::Theme;
use crate::widget::Button;

impl<M: ListModel + 'static> VirtualList<M> {
    /// The list's rows as elements: a flat list where the list is not a tree, and otherwise
    /// each row's notes nested in a `<ul>` inside the row's own element, with a branch row's
    /// disclosure standing where its chevron does.
    pub(crate) fn tree_html_rows(&self, rows: Vec<Composed>, theme: &Theme) -> Vec<Composed> {
        let Some(marks) = self.tree.as_ref() else {
            return rows;
        };
        // Pair each composed row with the row the list holds, by where it stands: the
        // composition names a row by its path in the window's layout, the list by its key.
        let mut standing: Vec<(u32, M::Key, Composed)> = Vec::with_capacity(rows.len());
        let mut loose: Vec<Composed> = Vec::new();
        for row in rows {
            let mounted = self.mounted.iter().find(|mounted| {
                let rect = mounted.widget.get_rect();
                near(rect.x0, row.rect[0])
                    && near(rect.y0, row.rect[1])
                    && near(rect.width(), row.rect[2])
                    && near(rect.height(), row.rect[3])
            });
            match mounted {
                Some(mounted) => {
                    standing.push((marks.depth(&mounted.key), mounted.key.clone(), row))
                }
                None => loose.push(row),
            }
        }
        for (_, key, row) in &mut standing {
            let Some(rect) = self.disclosure_rect(key) else {
                continue;
            };
            let folded = marks.is_folded(key);
            let part = self.disclosure_html(key, &row.path, rect, folded, theme);
            row.children.insert(0, part);
        }
        let mut rows = standing
            .into_iter()
            .map(|(depth, _, row)| (depth, row))
            .peekable();
        let base = rows.peek().map_or(0, |(depth, _)| *depth);
        let mut nested = nest_rows(&mut rows, base);
        // A row the mounted window does not reach stands as it composed, after the tree.
        nested.extend(rows.map(|(_, row)| row));
        nested.extend(loose);
        nested
    }

    /// A branch row's disclosure as an element: a button named for what it does, standing where
    /// the chevron is painted, pressing the list's own fold on the row it belongs to.
    fn disclosure_html(
        &self,
        key: &M::Key,
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
            format!("{row_path}/disclosure"),
            "RowDisclosure",
            [rect.x0, rect.y0, rect.width(), rect.height()],
        );
        part.tag = Some("button");
        part.focusable = true;
        part.own = Button::icon_only("", icon, &name)
            .composed_face(theme)
            .map(Own::Face);
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

/// The rows as elements, each row's notes nested in a `<ul>` of their own inside it: `depth` is
/// the level the rows at this call stand at.
fn nest_rows(
    rows: &mut Peekable<impl Iterator<Item = (u32, Composed)>>,
    depth: u32,
) -> Vec<Composed> {
    let mut out = Vec::new();
    while rows.peek().is_some_and(|(at, _)| *at == depth) {
        let Some((_, mut row)) = rows.next() else {
            break;
        };
        if let Some(deeper) = rows.peek().map(|(at, _)| *at).filter(|at| *at > depth) {
            let children = nest_rows(rows, deeper);
            let notes = group(&row.path, row.rect, children);
            row.children.push(notes);
        }
        out.push(row);
    }
    out
}

/// `children` in a group of their own under `parent`, standing over the region they take.
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
        format!("{parent}/notes"),
        "TreeNotes",
        [rect.x0, rect.y0, rect.width(), rect.height()],
    );
    group.tag = Some("ul");
    group.children = children;
    group
}

fn near(left: f64, right: f64) -> bool {
    (left - right).abs() < 0.5
}
