//! `Section`: a titled group of content on the ground, separated from the
//! next group by space, never boxed.
//!
//! A section is an intrinsic-height stack whose items sit `rhythm.sibling`
//! apart. Consecutive [`ListRow`]s are one list: they sit edge to edge with
//! an inset separator between them, hidden beside the selected row. The
//! optional header carries the group's title (sentence case, in `Heading`
//! for a section of a page or `Subheading` for a group inside one), a count
//! of what it holds, and one trailing text action ("Clear all", "Stage
//! all"), `rhythm.related` above the first item. There is no subtitle: a
//! description goes in the content only when it tells the person something
//! they need to act on.
//!
//! [`ListRow`]: crate::widget::ListRow

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::composition_parts::{Parts, retained_parts};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{Button, Element, TaffyTreeExt, Text, Widget};

/// How prominent a section's title is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Level {
    /// A section of a page: `Heading`.
    #[default]
    Section,
    /// A group inside a section or a list section header: `Subheading`.
    Group,
}

/// A section's header row.
pub struct SectionHeader {
    title: String,
    level: Level,
    count: Option<usize>,
    action: Option<Button>,
}

impl SectionHeader {
    /// A section of a page, titled in `Heading`.
    pub fn heading(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            level: Level::Section,
            count: None,
            action: None,
        }
    }

    /// A group inside a section (or a list's section header), titled in
    /// `Subheading`.
    pub fn group(title: impl Into<String>) -> Self {
        Self {
            level: Level::Group,
            ..Self::heading(title)
        }
    }

    /// How many items the section holds, in a quiet pill after the title.
    #[must_use]
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// One trailing text action that acts on the whole section.
    #[must_use]
    pub fn action(mut self, button: Button) -> Self {
        self.action = Some(button);
        self
    }

    fn into_widget(self) -> HeaderRow {
        let role = match self.level {
            Level::Section => TextRole::Heading,
            Level::Group => TextRole::Subheading,
        };
        let level = match self.level {
            Level::Section => 2,
            Level::Group => 3,
        };
        let mut children: Vec<Element> = vec![Box::new(
            Text::new(&self.title)
                .with_role(role)
                .with_heading_level(level),
        )];
        if let Some(count) = self.count {
            children.push(Box::new(CountPill {
                label: count.to_string(),
                rect: Rect::ZERO,
                padding: 0.0,
                style: None,
            }));
        }
        let has_action = self.action.is_some();
        if let Some(action) = self.action {
            children.push(Box::new(action.ghost().with_action_sizing()));
        }
        HeaderRow {
            parts: Parts::new(children),
            has_action,
        }
    }
}

/// A count in a quiet pill: `Caption` in tabular figures.
struct CountPill {
    label: String,
    rect: Rect,
    padding: f64,
    style: Option<crate::text_render::TextStyle>,
}

impl Widget for CountPill {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.padding = f64::from(theme.tokens.spacing.s2);
        self.style = Some(theme.text(TextRole::Caption).numeric());
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(style) = &self.style else {
            return taffy::Size::ZERO;
        };
        let (w, h) = text.measure_style(&self.label, None, style);
        let pad = crate::num::f32_of_f64(self.padding);
        taffy::Size {
            width: w + 2.0 * pad,
            height: h + pad / 2.0,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.height() <= 0.0 {
            return;
        }
        crate::surface::card(
            scene,
            transform,
            self.rect,
            self.rect.height() / 2.0,
            theme.tokens.colors.surfaces.raised,
        );
        let style = theme.text(TextRole::Caption).numeric();
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        let (w, h) = ctx.text.measure_style(&self.label, None, &style);
        let ink = theme.text_dim;
        ctx.text(
            &self.label,
            &style,
            ink,
            Point::new(
                self.rect.center().x - f64::from(w) / 2.0,
                self.rect.center().y - f64::from(h) / 2.0,
            ),
            None,
        );
    }

    fn name(&self) -> &'static str {
        "SectionCount"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::StaticText)
                .with_value(self.label.clone()),
        )
    }
}

struct HeaderRow {
    parts: Parts,
    has_action: bool,
}

impl Widget for HeaderRow {
    retained_parts!("SectionHeader");

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.parts.build(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                align_items: Some(taffy::AlignItems::Center),
                gap: Size {
                    width: length(theme.tokens.spacing.rhythm.related),
                    height: length(0.0_f32),
                },
                size: Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        // The title takes what is left and wraps; the action trails.
        if let Some(&title) = self.parts.nodes.first()
            && let Ok(style) = taffy.style(title)
        {
            let mut style = style.clone();
            style.flex_shrink = 1.0;
            style.min_size.width = length(0.0_f32);
            let _ = taffy.set_style(title, style);
        }
        if self.has_action
            && let Some(&action) = self.parts.nodes.last()
            && let Ok(style) = taffy.style(action)
        {
            let mut style = style.clone();
            style.margin.left = auto();
            let _ = taffy.set_style(action, style);
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

/// An intrinsic-height group of content with an optional header. See the
/// module docs.
pub struct Section {
    parts: Parts,
    has_header: bool,
}

impl Section {
    /// Stack `items` `rhythm.sibling` apart.
    pub fn new(items: Vec<Element>) -> Self {
        Self {
            parts: Parts::new(items),
            has_header: false,
        }
    }

    /// A section of a page titled `title`.
    pub fn titled(title: impl Into<String>, items: Vec<Element>) -> Self {
        Self::new(items).with_header(SectionHeader::heading(title))
    }

    /// Replace the header.
    #[must_use]
    pub fn with_header(mut self, header: SectionHeader) -> Self {
        let row: Element = Box::new(header.into_widget());
        if self.has_header
            && let Some(slot) = self.parts.children.first_mut()
        {
            *slot = row;
        } else {
            self.parts.children.insert(0, row);
            self.has_header = true;
        }
        self
    }
}

impl Widget for Section {
    crate::provides!(dyn crate::undo::UndoPoints);
    retained_parts!("Section");

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let rhythm = &theme.tokens.spacing.rhythm;
        let node = self.parts.build(
            Style {
                // One-column implicit Grid, as an intrinsic Column: every row
                // is sized by its content, so the section's height is exact
                // inside a scrolling stack.
                display: taffy::Display::Grid,
                grid_template_columns: vec![taffy::style_helpers::minmax(
                    length(0.0_f32),
                    taffy::style_helpers::fr(1.0_f32),
                )],
                align_content: Some(taffy::AlignContent::Start),
                size: Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                min_size: Size {
                    width: length(0.0_f32),
                    height: auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        // Each item's distance from the one above: `related` under the
        // header (it belongs to its items), nothing between consecutive list
        // rows (one list), `sibling` otherwise.
        let rows = self.row_flags();
        for (index, &node) in self.parts.nodes.iter().enumerate().skip(1) {
            let gap = if self.has_header && index == 1 {
                rhythm.related
            } else if rows.get(index - 1).copied().unwrap_or(false)
                && rows.get(index).copied().unwrap_or(false)
            {
                0.0
            } else {
                rhythm.sibling
            };
            if let Ok(style) = taffy.style(node) {
                let mut style = style.clone();
                style.margin.top = length(gap);
                let _ = taffy.set_style(node, style);
            }
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.sync_row_separators();
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

impl Section {
    /// Which items are list rows.
    fn row_flags(&mut self) -> Vec<bool> {
        self.parts
            .children
            .iter_mut()
            .map(|child| {
                child
                    .as_any_mut()
                    .is_some_and(|any| any.downcast_mut::<crate::widget::ListRow>().is_some())
            })
            .collect()
    }

    /// A list row gets a separator when a list row follows it, hidden when
    /// either is selected.
    fn sync_row_separators(&mut self) {
        let selected: Vec<Option<bool>> = self
            .parts
            .children
            .iter_mut()
            .map(|child| {
                child
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<crate::widget::ListRow>())
                    .map(|row| row.is_selected())
            })
            .collect();
        for (index, child) in self.parts.children.iter_mut().enumerate() {
            let next = selected.get(index + 1).copied().flatten();
            if let Some(row) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<crate::widget::ListRow>())
            {
                row.set_separator(next.is_some(), next.unwrap_or(false));
            }
        }
    }
}

impl crate::undo::UndoPoints for Section {
    fn needs_commit(&self) -> bool {
        self.parts
            .children
            .iter()
            .any(|c| crate::undo::needs_commit(c.as_ref()))
    }
    fn on_commit(&mut self) {
        for child in &mut self.parts.children {
            crate::undo::on_commit(child.as_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::{FrameHarness, InspectionNode};

    fn rect(node: &InspectionNode) -> Rect {
        let [x, y, w, h] = node.rect;
        Rect::new(x, y, x + w, y + h)
    }

    #[test]
    fn items_sit_a_sibling_step_apart_and_the_header_closer() {
        let theme = Theme::dark();
        let rhythm = theme.tokens.spacing.rhythm;
        let section = Section::new(
            (0..3)
                .map(|i| Box::new(Text::new(&format!("file {i}"))) as Element)
                .collect(),
        )
        .with_header(
            SectionHeader::heading("Changes")
                .count(3)
                .action(Button::new("stage-all", "Stage all", None)),
        );
        let harness =
            FrameHarness::new_with_theme(crate::State::new(), Box::new(section), (500, 400), theme);
        let root = harness.inspection().root;
        let header = &root.children[0];
        let items = &root.children[1..];
        assert!((rect(&items[0]).y0 - rect(header).y1 - f64::from(rhythm.related)).abs() < 0.5);
        assert!((rect(&items[1]).y0 - rect(&items[0]).y1 - f64::from(rhythm.sibling)).abs() < 0.5);
        // Title, count, then the action at the trailing edge.
        assert_eq!(header.children.len(), 3);
        let title = rect(&header.children[0]);
        let count = rect(&header.children[1]);
        let action = rect(&header.children[2]);
        assert!(count.x0 > title.x1);
        assert!((action.x1 - 500.0).abs() < 1.0, "action trails: {action:?}");
        assert!(
            (count.center().y - title.center().y).abs() < 1.0,
            "one centre line"
        );
    }

    #[test]
    fn list_rows_in_a_section_are_one_list() {
        let section = Section::titled(
            "People",
            vec![
                Box::new(crate::widget::ListRow::new("a", "Ada")),
                Box::new(crate::widget::ListRow::new("b", "Grace").selected(true)),
                Box::new(crate::widget::ListRow::new("c", "Linus")),
                Box::new(Text::new("3 people")),
            ],
        );
        let harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(section),
            (500, 400),
            Theme::dark(),
        );
        let root = harness.inspection().root;
        let rows: Vec<Rect> = root.children[1..4].iter().map(rect).collect();
        assert!((rows[1].y0 - rows[0].y1).abs() < 0.5, "rows touch");
        assert!((rows[2].y0 - rows[1].y1).abs() < 0.5);
        let footer = rect(&root.children[4]);
        let sibling = f64::from(Theme::dark().tokens.spacing.rhythm.sibling);
        assert!((footer.y0 - rows[2].y1 - sibling).abs() < 0.5);
        // Separators: under Ada hidden (Grace below is selected), under
        // Grace hidden (selected), none under Linus (no row follows).
        let mut section = Section::new(vec![
            Box::new(crate::widget::ListRow::new("a", "Ada")),
            Box::new(crate::widget::ListRow::new("b", "Grace").selected(true)),
            Box::new(crate::widget::ListRow::new("c", "Linus")),
            Box::new(crate::widget::ListRow::new("d", "Mary")),
        ]);
        section.sync_row_separators();
        let flags: Vec<(bool, bool)> = section
            .parts
            .children
            .iter_mut()
            .map(|c| {
                let row = c
                    .as_any_mut()
                    .and_then(|a| a.downcast_mut::<crate::widget::ListRow>())
                    .unwrap();
                row.separator_state()
            })
            .collect();
        assert_eq!(
            flags,
            [(true, true), (true, false), (true, false), (false, false)]
        );
    }

    #[test]
    fn headings_take_their_role_and_follow_the_text_scale() {
        let height = |header: SectionHeader, scale: f32| {
            let harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(Section::new(vec![Box::new(Text::new("body"))]).with_header(header)),
                (400, 300),
                Theme::dark().with_text_scale(scale),
            );
            harness.inspection().root.children[0].children[0].rect[3]
        };
        let theme = Theme::dark();
        let heading = height(SectionHeader::heading("Library"), 1.0);
        let group = height(SectionHeader::group("Recently added"), 1.0);
        assert!((heading - f64::from(theme.text(TextRole::Heading).line_height_px())).abs() < 1.0);
        assert!((group - f64::from(theme.text(TextRole::Subheading).line_height_px())).abs() < 1.0);
        assert!(height(SectionHeader::heading("Library"), 2.0) > heading * 1.8);
        // No header, no edge spacing.
        let plain = Section::new(vec![Box::new(Text::new("one"))]);
        assert_eq!(plain.parts.children.len(), 1);
    }
}
