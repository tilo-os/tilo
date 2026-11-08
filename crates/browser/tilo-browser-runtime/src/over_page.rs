//! A sheet drawn over the page, out of its flow: the session's own sheet,
//! kept current by whoever answers it, taking the keyboard and the press that
//! dismisses it. The page shows two — the directory and the invite sheet —
//! and how a sheet stands over a page is the same for both, so it is here
//! once and each answers [`OverSheet`] with its own sheet and effects.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::{LengthPercentageAuto, NodeId, Position, Style, TaffyTree};
use ui::vello::Scene;
use ui::widget::{AppModel, Element, EventCtx, ModelPath, TaffyTreeExt as _, Widget};
use ui::winit::event::WindowEvent;
use ui::{TextRenderer, Theme};

/// What a sheet the page draws over its desktop asks of the page: the sheet as
/// it stands after taking what arrived, and what the person pressed while it
/// was up.
pub(crate) trait OverSheet {
    /// Take what arrived, hand the sheet as it stands to `show` when it moved
    /// or when the page holds nothing of it (`missing`: it was down, or this
    /// is the first pass), and carry out what the person pressed since.
    /// Whether anything changed.
    fn follow_sheet(&mut self, show: &mut dyn FnMut(Option<Element>), missing: bool) -> bool;

    /// Whether the sheet is up, taking the keyboard and what it covers.
    fn drawn(&self) -> bool;

    /// What this part of the page is called.
    fn sheet_name(&self) -> &'static str;
}

/// `sheet`, drawn over the whole page: out of the page's flow, with the sheet
/// it shows as its only child.
pub(crate) struct OverPage<S> {
    sheet: S,
    content: Option<Element>,
    /// The sheet was built again since the page last solved its layout.
    replaced: bool,
    rect: Rect,
}

impl<S> OverPage<S> {
    pub(crate) fn new(sheet: S) -> Self {
        Self {
            sheet,
            content: None,
            replaced: false,
            rect: Rect::ZERO,
        }
    }

    /// Take what arrived and show the sheet as it stands. Whether anything
    /// changed.
    pub(crate) fn follow(&mut self) -> bool
    where
        S: OverSheet,
    {
        let Self {
            sheet,
            content,
            replaced,
            ..
        } = self;
        let mut rebuilt = false;
        let missing = content.is_none();
        // A sheet put away leaves nothing of it on the page, and the page
        // draws again without it.
        let changed = sheet.follow_sheet(
            &mut |shown| {
                rebuilt |= shown.is_some() || content.is_some();
                *content = shown;
            },
            missing,
        );
        *replaced |= rebuilt;
        changed | rebuilt
    }
}

impl<S> Deref for OverPage<S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.sheet
    }
}

impl<S> DerefMut for OverPage<S> {
    fn deref_mut(&mut self) -> &mut S {
        &mut self.sheet
    }
}

impl<S: OverSheet + 'static> Widget for OverPage<S> {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.follow()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let content = self
            .content
            .as_mut()
            .map(|content| content.build_layout(taffy, theme, map));
        // Over the whole page, out of its flow: the sheet's scrim covers it.
        let style = Style {
            position: Position::Absolute,
            inset: ui::taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                bottom: LengthPercentageAuto::length(0.0),
            },
            ..Default::default()
        };
        let children: Vec<NodeId> = content.into_iter().collect();
        taffy.widget_node(style, &children, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let Some(content) = self.content.as_mut()
            && let Some(child) = taffy.children(node).unwrap_or_default().first().copied()
        {
            content.apply_layout(taffy, child, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(content) = self.content.as_mut() {
            content.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let Some(content) = self.content.as_mut() else {
            return;
        };
        let Some(&child) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() else {
            return;
        };
        content.handle_event(event, &mut ctx.reborrow(ctx.path, child));
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.content.as_deref().into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match self.content.as_mut() {
            Some(content) => vec![content.as_mut()],
            None => Vec::new(),
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.replaced)
            | self
                .content
                .as_mut()
                .is_some_and(|content| content.take_structure_dirty())
    }

    /// A page with no sheet up has nothing under it whose size could be
    /// solved again: the default answers for every childless widget with
    /// "measure", which would re-solve the whole tree on each frame that
    /// lays out, a pan's included.
    fn layout_needs_measure(&self) -> bool {
        self.content
            .as_ref()
            .is_some_and(|content| content.layout_needs_measure())
    }

    fn traps_focus(&self) -> bool {
        self.sheet.drawn()
    }

    fn needs_redraw(&self) -> bool {
        self.content
            .as_ref()
            .is_some_and(|content| content.needs_redraw())
    }

    fn name(&self) -> &'static str {
        self.sheet.sheet_name()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
