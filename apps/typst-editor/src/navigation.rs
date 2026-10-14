//! Project files and the current source's outline, in the shared navigation
//! list. The drawer starts closed so opening a workspace keeps both panes' room.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use typst::syntax::{LinkedNode, Source, SyntaxKind, SyntaxNode, ast};
use ui::i18n::EmbeddedCatalog;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::Resource;
use ui::tilo_resource::{Effect, Id};
use ui::widget::{Decorator, NavItem, NavList, NavSection, SidebarDrawer, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::actions::TypstEffect;
use crate::resource::Workspace;

// Kept with this self-contained component; these keys share the app's locale.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[(
        "en-US",
        "typst-navigation-name = Project navigation\ntypst-navigation-files = Files\ntypst-navigation-outline = Outline\n",
    )],
);

/// A literal markup heading. Lines are one-based, as in `SourceEditor::goto_line`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Heading {
    pub(crate) line: usize,
    pub(crate) level: usize,
    pub(crate) title: String,
}

/// Read syntax rather than matching lines: raw examples and commented-out
/// headings are not destinations. Dynamic headings require compilation and
/// are deliberately not guessed by the source outline.
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
        use typst::syntax::ast::AstNode;
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

/// Keep readable, static markup, not its delimiters, labels or code strings.
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

/// Count prose rather than settings, comments and math syntax.
pub(crate) fn word_count(source: &str) -> usize {
    fn prose(node: &SyntaxNode, output: &mut String) {
        match node.kind() {
            SyntaxKind::Text => output.push_str(node.leaf_text()),
            SyntaxKind::Space | SyntaxKind::Parbreak | SyntaxKind::Linebreak => output.push(' '),
            SyntaxKind::Raw
            | SyntaxKind::LineComment
            | SyntaxKind::BlockComment
            | SyntaxKind::Label => {}
            _ => {
                for child in node.children() {
                    prose(child, output);
                }
            }
        }
    }
    let source = Source::detached(source);
    let mut text = String::new();
    prose(source.root(), &mut text);
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphabetic))
        .count()
}

fn heading_id(line: usize) -> String {
    format!("typst.heading:{line}")
}

fn file_id(id: &Id) -> String {
    format!("typst.project-file:{id}")
}

/// Each heading belongs to the nearest preceding shallower heading, including
/// when the source skips levels. There are no fabricated intermediate rows.
fn outline_items(headings: &[Heading]) -> Vec<NavItem> {
    fn children(
        remaining: &mut std::iter::Peekable<std::slice::Iter<'_, Heading>>,
        parent_level: usize,
    ) -> Vec<NavItem> {
        let mut items = Vec::new();
        while remaining
            .peek()
            .is_some_and(|next| next.level > parent_level)
        {
            let Some(heading) = remaining.next() else {
                break;
            };
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

pub(crate) struct Navigation {
    drawer: SidebarDrawer,
    list: NavList,
    requests: Rc<RefCell<Vec<String>>>,
    sections: Vec<NavSection>,
    destinations: HashMap<String, TypstEffect>,
    current: Option<Id>,
    source: Option<String>,
    headings: Vec<Heading>,
    structure_dirty: bool,
}

impl Navigation {
    pub(crate) fn new() -> Self {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let chosen = Rc::clone(&requests);
        Self {
            drawer: SidebarDrawer::new().left().closed(),
            list: NavList::new(Vec::new())
                .with_id("typst.navigation")
                .with_accessible_name(CATALOG.t("typst-navigation-name"))
                .on_activate(move |id| chosen.borrow_mut().push(id)),
            requests,
            sections: Vec::new(),
            destinations: HashMap::new(),
            current: None,
            source: None,
            headings: Vec::new(),
            structure_dirty: true,
        }
    }

    /// Chosen destinations are local navigation intents; the host dispatches
    /// their published effects with its record's own custody and invoker.
    pub(crate) fn take_requests(&self) -> Vec<TypstEffect> {
        std::mem::take(&mut *self.requests.borrow_mut())
            .into_iter()
            .filter_map(|id| self.destinations.get(&id).cloned())
            .collect()
    }

    pub(crate) fn sync(&mut self, workspace: &Workspace, current: Option<&Id>, source: &str) {
        if self.source.as_deref() != Some(source) {
            self.headings = headings(source);
            self.source = Some(source.to_owned());
        }
        let mut destinations = HashMap::new();
        let files = workspace
            .files
            .iter()
            .map(|(id, file)| {
                let key = file_id(id);
                destinations.insert(
                    key.clone(),
                    TypstEffect::OpenProjectFile { id: id.to_string() },
                );
                NavItem::new(key, &file.name.0)
            })
            .collect();
        for heading in &self.headings {
            destinations.insert(
                heading_id(heading.line),
                TypstEffect::GoToHeading { line: heading.line },
            );
        }
        let sections = vec![
            NavSection::new("typst.files", CATALOG.t("typst-navigation-files"), files),
            NavSection::new(
                "typst.outline",
                CATALOG.t("typst-navigation-outline"),
                outline_items(&self.headings),
            ),
        ];
        if sections != self.sections {
            self.list.set_sections(sections.clone());
            self.sections = sections;
        }
        if destinations != self.destinations {
            self.destinations = destinations;
        }
        if self.current.as_ref() != current || self.list.selected_id().is_none() {
            self.list.set_selected(current.map(file_id).as_deref());
        }
        self.current = current.cloned();
    }

    pub(crate) fn layout(&mut self, body: Rect, theme: &Theme) -> Rect {
        self.drawer.scale_for(theme);
        // Keep navigation compact at UI zoom; enlarged text still gets room.
        let width = f64::from(theme.tokens.metrics.sidebar_min_width + theme.tokens.spacing.s2)
            / f64::from(theme.ui_scale)
            * f64::from(theme.text_growth());
        self.drawer
            .set_docked_width(width / theme.responsive_scale());
        let remaining = self.drawer.resolve(body);
        self.list.inner_mut().place_at(self.drawer.list_rect());
        remaining
    }

    /// Snap rather than animate: the painted host does not need an idle tick
    /// or a second layout between its content and this drawer.
    pub(crate) fn toggle(&mut self) {
        self.drawer.toggle();
        self.drawer.snap(self.drawer.is_open());
    }

    /// Also suitable for deterministic captures before the first layout.
    pub(crate) fn open(&mut self) {
        self.drawer.open();
        self.drawer.snap(true);
    }

    pub(crate) fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    pub(crate) fn visible(&self) -> bool {
        self.drawer.is_open()
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<String> {
        self.list.selected_id()
    }

    pub(crate) fn rect(&self) -> Rect {
        self.drawer.sidebar_rect()
    }

    pub(crate) fn widget(&self) -> &dyn Widget {
        &self.list
    }

    pub(crate) fn widget_mut(&mut self) -> &mut dyn Widget {
        &mut self.list
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
        self.drawer.draw_scrim(scene, transform, theme);
        let panel_theme = self.drawer.paint_panel(scene, transform, theme);
        Widget::draw(&mut self.list, scene, transform, tr, &panel_theme, presence);
    }

    /// Address a heading by its row id or its readable title. Hidden/folded
    /// rows have no pointer target; their published action remains available.
    pub(crate) fn heading_target(&self, title_or_id: &str) -> Option<Rect> {
        if !self.visible() {
            return None;
        }
        self.headings
            .iter()
            .find(|heading| heading.title == title_or_id || heading_id(heading.line) == title_or_id)
            .and_then(|heading| self.list.row_rect(&heading_id(heading.line)))
    }

    pub(crate) fn dismiss_if_overlay(&mut self) {
        if self.drawer.is_overlay() {
            self.drawer.close();
            self.drawer.snap(false);
        }
    }

    pub(crate) fn dismiss_at(&mut self, point: Point) -> bool {
        if self.drawer.press_dismisses(point) {
            self.drawer.snap(false);
            true
        } else {
            false
        }
    }

    /// Publish the same destinations and hierarchy as the navigation list.
    pub(crate) fn live_resources(&self) -> Vec<Resource> {
        fn row(item: &NavItem, destinations: &HashMap<String, TypstEffect>) -> Resource {
            let kind = if item.id.starts_with("typst.heading:") {
                "typst.heading"
            } else {
                "typst.project-file"
            };
            let resource = Resource::new(&item.id, kind, &item.label);
            let resource = match destinations.get(&item.id) {
                Some(effect) => resource.with_action(effect.bound()),
                None => resource,
            };
            resource.with_children(item.children.iter().map(|child| row(child, destinations)))
        }
        self.sections
            .iter()
            .map(|section| {
                Resource::new(&section.id, &section.id, &section.title).with_children(
                    section
                        .items
                        .iter()
                        .map(|item| row(item, &self.destinations)),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_prose_not_directives_comments_or_math() {
        assert_eq!(
            word_count(
                "#set text(font: \"Example Font\")\n// hidden words\n= Café\nA *small* note. $x + y$"
            ),
            4
        );
    }

    #[test]
    fn parses_levels_unicode_and_one_based_lines() {
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
    }

    #[test]
    fn ignores_comments_raw_and_code_strings() {
        let source = "// = Comment\n/*\n== Block comment\n*/\n```typ\n= Raw example\n```\n#let example = \"= String\"\n= Real\n";
        assert_eq!(
            headings(source),
            vec![Heading {
                line: 9,
                level: 1,
                title: "Real".into(),
            }]
        );
        assert!(headings("`= inline raw`\nordinary = text\n\\= escaped").is_empty());
    }

    #[test]
    fn strips_markup_comments_and_labels_from_titles() {
        assert_eq!(
            headings("= *Bold* _italic_ `code` \\# café <intro>\n"),
            vec![Heading {
                line: 1,
                level: 1,
                title: "Bold italic code # café".into(),
            }]
        );
        assert_eq!(
            headings("= Hello /* hidden */ world\n")
                .first()
                .unwrap()
                .title,
            "Hello world"
        );
    }

    #[test]
    fn handles_crlf_and_nested_markup() {
        assert_eq!(
            headings("é\r\n#block[\r\n== Inside\r\n]\r\n= Outside"),
            vec![
                Heading {
                    line: 3,
                    level: 2,
                    title: "Inside".into()
                },
                Heading {
                    line: 5,
                    level: 1,
                    title: "Outside".into()
                },
            ]
        );
    }

    #[test]
    fn hierarchy_uses_nearest_shallower_heading_without_fake_rows() {
        let items = outline_items(&headings(
            "== First\n==== Deep\n=== Middle\n= Root\n== Child\n= Last",
        ));
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["First", "Root", "Last"]
        );
        assert_eq!(
            items[0]
                .children
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Deep", "Middle"]
        );
        assert_eq!(items[1].children[0].label, "Child");
        assert!(items[2].children.is_empty());
    }

    #[test]
    fn sync_preserves_folded_branches_when_the_outline_changes() {
        let mut navigation = Navigation::new();
        let workspace = Workspace::default();
        navigation.sync(&workspace, None, "= Root\n== Child");
        navigation.list.set_expanded(&heading_id(1), false);
        navigation.sync(&workspace, None, "= Root\n== Renamed child\n= Next");
        assert!(!navigation.list.is_expanded(&heading_id(1)));
        assert_eq!(
            navigation.list.visible_item_ids(),
            vec![heading_id(1), heading_id(3)]
        );
    }

    #[test]
    fn empty_source_has_no_outline_and_drawer_starts_closed() {
        assert!(headings("").is_empty());
        let mut navigation = Navigation::new();
        let body = Rect::new(0.0, 0.0, 1200.0, 800.0);
        assert_eq!(navigation.layout(body, &Theme::dark()), body);
        assert!(!navigation.visible());
        assert_eq!(navigation.heading_target("Missing"), None);
    }
}
