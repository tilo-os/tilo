//! The open document's outline: its headings in the shared navigation list,
//! in a drawer beside the source. Choosing a heading runs the published
//! `go-to-heading` effect, so a person's click and an agent's call land on
//! the same line.

use std::cell::RefCell;
use std::rc::Rc;

use typst::syntax::{LinkedNode, Source, SyntaxKind, SyntaxNode, ast};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::Resource;
use ui::tilo_resource::Effect;
use ui::widget::{NavItem, NavList, NavSection, SidebarDrawer, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::actions::TypstEffect;

/// A heading written in the source. Lines are one-based, as
/// `SourceEditor::goto_line` takes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Heading {
    pub(crate) line: usize,
    pub(crate) level: usize,
    pub(crate) title: String,
}

/// The headings `source` writes, read from its syntax rather than its lines:
/// a heading in raw text, a comment or a string is not one. Headings a
/// script makes only exist once compiled, and are not guessed at.
pub(crate) fn headings(source: &str) -> Vec<Heading> {
    let source = Source::detached(source);
    let mut result = Vec::new();
    collect_headings(&LinkedNode::new(source.root()), &source, &mut result);
    result
}

fn collect_headings(node: &LinkedNode<'_>, source: &Source, result: &mut Vec<Heading>) {
    if matches!(
        node.get().kind(),
        SyntaxKind::Raw | SyntaxKind::LineComment | SyntaxKind::BlockComment
    ) {
        return;
    }
    if let Some(heading) = node.get().cast::<ast::Heading<'_>>()
        && let Some(line) = source.lines().byte_to_line(node.range().start)
    {
        use typst::syntax::ast::AstNode as _;
        let mut title = String::new();
        collect_title(heading.body().to_untyped(), &mut title);
        result.push(Heading {
            line: line + 1,
            level: heading.depth().get(),
            title: title.split_whitespace().collect::<Vec<_>>().join(" "),
        });
    }
    for child in node.children() {
        collect_headings(&child, source, result);
    }
}

/// The heading's readable text: its markup without delimiters, labels or
/// comments.
fn collect_title(node: &SyntaxNode, title: &mut String) {
    match node.kind() {
        SyntaxKind::Text | SyntaxKind::MathText | SyntaxKind::SmartQuote => {
            title.push_str(node.leaf_text());
        }
        SyntaxKind::Space | SyntaxKind::Parbreak | SyntaxKind::Linebreak => title.push(' '),
        SyntaxKind::Escape => {
            if let Some(escape) = node.cast::<ast::Escape<'_>>() {
                title.push(escape.get());
            }
        }
        SyntaxKind::Shorthand => {
            if let Some(shorthand) = node.cast::<ast::Shorthand<'_>>() {
                title.push(shorthand.get());
            }
        }
        SyntaxKind::Raw => {
            if let Some(raw) = node.cast::<ast::Raw<'_>>() {
                for line in raw.lines() {
                    title.push_str(line.get());
                }
            }
        }
        SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::Label => {}
        _ => {
            for child in node.children() {
                collect_title(child, title);
            }
        }
    }
}

fn heading_id(line: usize) -> String {
    format!("typst.heading:{line}")
}

fn heading_line(id: &str) -> Option<usize> {
    id.strip_prefix("typst.heading:")?.parse().ok()
}

/// Each heading under the nearest shallower one before it, also when the
/// source skips a level; no rows are made up for the skipped levels.
fn outline_items(headings: &[Heading]) -> Vec<NavItem> {
    fn children(
        remaining: &mut std::iter::Peekable<std::slice::Iter<'_, Heading>>,
        parent_level: usize,
    ) -> Vec<NavItem> {
        let mut items = Vec::new();
        while let Some(heading) = remaining.next_if(|next| next.level > parent_level) {
            let nested = children(remaining, heading.level);
            items.push(
                NavItem::new(heading_id(heading.line), &heading.title)
                    .children(nested)
                    .expanded(),
            );
        }
        items
    }
    children(&mut headings.iter().peekable(), 0)
}

pub(crate) struct Outline {
    drawer: SidebarDrawer,
    list: NavList,
    /// The heading row chosen since the host last looked.
    chosen: Rc<RefCell<Option<String>>>,
    source: Option<String>,
    headings: Vec<Heading>,
    /// The drawer opened or closed since the host's layout tree was built:
    /// the list joins the window's children only while it shows.
    structure_dirty: bool,
}

impl Outline {
    pub(crate) fn new() -> Self {
        let chosen = Rc::new(RefCell::new(None));
        let pick = Rc::clone(&chosen);
        Self {
            drawer: SidebarDrawer::new().left().closed(),
            list: NavList::new(Vec::new())
                .with_id("typst.outline")
                .with_accessible_name("Outline")
                .on_select(move |id| *pick.borrow_mut() = Some(id)),
            chosen,
            source: None,
            headings: Vec::new(),
            structure_dirty: false,
        }
    }

    /// Follow the open document's source.
    pub(crate) fn sync(&mut self, source: &str) {
        if self.source.as_deref() == Some(source) {
            return;
        }
        let headings = headings(source);
        self.source = Some(source.to_owned());
        if headings != self.headings {
            self.list.set_sections(vec![
                NavSection::new("typst.outline", "Outline", outline_items(&headings))
                    .empty_caption("No headings"),
            ]);
            self.headings = headings;
        }
    }

    /// The heading the person chose since the last call, as the effect that
    /// goes to it. The row does not stay selected, so choosing the same
    /// heading again goes there again.
    pub(crate) fn take_chosen(&mut self) -> Option<TypstEffect> {
        let id = self.chosen.borrow_mut().take()?;
        self.list.set_selected(None);
        heading_line(&id).map(|line| TypstEffect::GoToHeading { line })
    }

    /// Size the drawer for `theme`'s scale.
    pub(crate) fn scale_for(&mut self, theme: &Theme) {
        self.drawer.scale_for(theme);
    }

    /// Lay the drawer out in `body`; returns what it leaves the panes.
    pub(crate) fn layout(&mut self, body: Rect) -> Rect {
        let remaining = self.drawer.resolve(body);
        self.list.place_at(self.drawer.list_rect());
        remaining
    }

    pub(crate) fn toggle(&mut self) {
        self.drawer.toggle();
        self.drawer.snap(self.drawer.is_open());
        self.structure_dirty = true;
    }

    /// Whether the list joined or left the window since the last call.
    pub(crate) fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) | self.list.take_structure_dirty()
    }

    pub(crate) fn visible(&self) -> bool {
        self.drawer.is_open()
    }

    pub(crate) fn rect(&self) -> Rect {
        self.drawer.sidebar_rect()
    }

    /// The list, while the drawer shows.
    pub(crate) fn widget(&self) -> Option<&dyn Widget> {
        self.visible().then_some(&self.list as &dyn Widget)
    }

    /// The list, while the drawer shows.
    pub(crate) fn widget_mut(&mut self) -> Option<&mut dyn Widget> {
        if self.visible() {
            Some(&mut self.list)
        } else {
            None
        }
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.visible() {
            return;
        }
        let panel = self.drawer.paint_panel(scene, transform, theme);
        Widget::draw(&mut self.list, scene, transform, tr, &panel, presence);
    }

    /// A floating drawer gets out of the way once a heading was chosen.
    pub(crate) fn dismiss_if_overlay(&mut self) {
        if self.drawer.is_overlay() {
            self.drawer.close();
            self.drawer.snap(false);
            self.structure_dirty = true;
        }
    }

    /// A press on a floating drawer's scrim closes it.
    pub(crate) fn dismiss_at(&mut self, point: Point) -> bool {
        if self.drawer.press_dismisses(point) {
            self.drawer.snap(false);
            self.structure_dirty = true;
            true
        } else {
            false
        }
    }

    /// The headings as resources, each with the effect that goes to it.
    pub(crate) fn resources(&self) -> Vec<Resource> {
        fn row(heading: &Heading) -> Resource {
            Resource::new(heading_id(heading.line), "typst.heading", &heading.title)
                .with_action(TypstEffect::GoToHeading { line: heading.line }.bound())
        }
        if self.headings.is_empty() {
            return Vec::new();
        }
        vec![
            Resource::new("typst.outline", "typst.outline", "Outline")
                .with_children(self.headings.iter().map(row)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_are_read_from_syntax_with_one_based_lines() {
        assert_eq!(
            headings("intro\n= Café λ\n\n=== Deep\n== Sibling\n"),
            vec![
                Heading {
                    line: 2,
                    level: 1,
                    title: "Café λ".into()
                },
                Heading {
                    line: 4,
                    level: 3,
                    title: "Deep".into()
                },
                Heading {
                    line: 5,
                    level: 2,
                    title: "Sibling".into()
                },
            ]
        );
        let ignored = "// = Comment\n/*\n== Block comment\n*/\n```typ\n= Raw example\n```\n#let example = \"= String\"\n= Real\n";
        assert_eq!(
            headings(ignored),
            vec![Heading {
                line: 9,
                level: 1,
                title: "Real".into(),
            }]
        );
        assert_eq!(
            headings("= *Bold* _italic_ `code` \\# café <intro>\n")[0].title,
            "Bold italic code # café"
        );
        assert_eq!(
            headings("é\r\n#block[\r\n== Inside\r\n]\r\n= Outside")
                .iter()
                .map(|heading| heading.line)
                .collect::<Vec<_>>(),
            [3, 5]
        );
        assert!(headings("").is_empty());
    }

    #[test]
    fn each_heading_nests_under_the_nearest_shallower_one() {
        let items = outline_items(&headings(
            "== First\n==== Deep\n=== Middle\n= Root\n== Child\n= Last",
        ));
        let labels = |items: &[NavItem]| {
            items
                .iter()
                .map(|item| item.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(&items), ["First", "Root", "Last"]);
        assert_eq!(labels(&items[0].children), ["Deep", "Middle"]);
        assert_eq!(labels(&items[1].children), ["Child"]);
    }

    #[test]
    fn a_chosen_heading_is_the_effect_that_goes_to_it_and_does_not_stay_chosen() {
        let mut outline = Outline::new();
        outline.sync("= Root\n== Child");
        *outline.chosen.borrow_mut() = Some(heading_id(2));
        assert_eq!(
            outline.take_chosen(),
            Some(TypstEffect::GoToHeading { line: 2 })
        );
        assert_eq!(outline.list.selected_id(), None);
        assert_eq!(outline.take_chosen(), None);
        let resources = outline.resources();
        assert_eq!(resources[0].children.len(), 2);
        assert_eq!(resources[0].children[1].label, "Child");
    }

    #[test]
    fn the_drawer_starts_closed_and_takes_no_room() {
        let mut outline = Outline::new();
        let body = Rect::new(0.0, 0.0, 1200.0, 800.0);
        outline.scale_for(&Theme::dark());
        assert_eq!(outline.layout(body), body);
        assert!(!outline.visible());
        outline.toggle();
        let remaining = outline.layout(body);
        assert!(outline.visible());
        assert!(remaining.width() < body.width());
        assert!(outline.rect().width() > 0.0);
    }
}
