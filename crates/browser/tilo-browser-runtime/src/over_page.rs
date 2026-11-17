//! A sheet drawn over the page, out of its flow: the session's own sheet,
//! kept current by whoever answers it, taking the keyboard and the press that
//! dismisses it. The page shows several — the directory, the invite sheet,
//! the workspace settings — and how a sheet stands over a page is the same for
//! each, so it is here once and each answers [`OverSheet`] with its own sheet
//! and effects.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::{LengthPercentageAuto, NodeId, Position, Style, TaffyTree};
use ui::vello::Scene;
use ui::widget::DrawChild;
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

    /// The page drew nothing of the sheet: a sheet that is not open has finished leaving, and
    /// says so, as the shell that drew it out did over its own card. A sheet the page draws out
    /// itself says nothing here (the default): its own card's exit is its account.
    fn put_away(&self) {}

    /// Whether the sheet is up, taking the keyboard and what it covers.
    fn drawn(&self) -> bool;

    /// What this part of the page is called.
    fn sheet_name(&self) -> &'static str;
}

/// A sheet the person opens: the directory, the invite sheet and the workspace settings.
pub(crate) trait PageSheet: OverSheet {
    fn open(&self);
    fn is_open(&self) -> bool;
}

/// The sheets' code: the session's directory, invite and workspace settings sheets and the
/// workspace directory's store behind both, and the shortcut guide's sheet.
/// The browser desktop fetches it the first time the person opens one
/// (`deferred-sheets`).
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy)]
pub struct SheetsCode {
    directory: MakeDirectory,
    invite: MakeInvite,
    workspace_settings: MakeWorkspaceSettings,
    pub(crate) guide: FollowGuide,
}

/// Build the shortcut guide's sheet as it stands ([`OverSheet::follow_sheet`]).
#[cfg(target_arch = "wasm32")]
pub(crate) type FollowGuide =
    fn(&mut crate::guide::PageGuide, &mut dyn FnMut(Option<Element>), bool) -> bool;

#[cfg(target_arch = "wasm32")]
type MakeDirectory = fn(
    crate::shell::HostRequests,
    Option<&crate::directory::PageDirectoryConfig>,
    &Wake,
) -> Box<dyn PageSheet>;

#[cfg(target_arch = "wasm32")]
type MakeInvite = fn(
    crate::shell::HostRequests,
    Option<&crate::invite::PageInviteConfig>,
    crate::desktop::Shared,
    tilo_desktop::here::Here,
    Option<u32>,
    &Wake,
) -> Box<dyn PageSheet>;

#[cfg(target_arch = "wasm32")]
type MakeWorkspaceSettings = fn(
    crate::desktop::Shared,
    tilo_desktop::here::Here,
    Option<u32>,
    Option<crate::workspace_settings::PageRecords>,
) -> Box<dyn PageSheet>;

#[cfg(target_arch = "wasm32")]
type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// The sheets' code where it is linked.
#[must_use]
#[cfg(target_arch = "wasm32")]
pub fn builtin_sheets() -> SheetsCode {
    SheetsCode {
        directory: crate::directory::make,
        invite: crate::invite::make,
        workspace_settings: crate::workspace_settings::make,
        guide: crate::guide::follow,
    }
}

#[cfg(target_arch = "wasm32")]
ui::lazy_code! {
    /// The sheets' code.
    pub static SHEETS: SheetsCode = builtin_sheets, feature = "deferred-sheets";
}

/// A sheet made the first time the person opens it: until its code is in,
/// the open waits for it.
#[cfg(target_arch = "wasm32")]
pub(crate) struct LazySheet {
    make: Box<dyn Fn(SheetsCode) -> Box<dyn PageSheet>>,
    sheet: Option<Box<dyn PageSheet>>,
    asked: std::cell::Cell<bool>,
}

#[cfg(target_arch = "wasm32")]
impl LazySheet {
    pub(crate) fn directory(
        requests: crate::shell::HostRequests,
        config: Option<&crate::directory::PageDirectoryConfig>,
        wake: &Wake,
    ) -> Self {
        let (config, wake) = (config.cloned(), std::sync::Arc::clone(wake));
        Self::new(Box::new(move |code| {
            (code.directory)(std::rc::Rc::clone(&requests), config.as_ref(), &wake)
        }))
    }

    pub(crate) fn invite(
        requests: crate::shell::HostRequests,
        config: Option<&crate::invite::PageInviteConfig>,
        shared: crate::desktop::Shared,
        here: tilo_desktop::here::Here,
        peer: Option<u32>,
        wake: &Wake,
    ) -> Self {
        let (config, wake) = (config.cloned(), std::sync::Arc::clone(wake));
        Self::new(Box::new(move |code| {
            (code.invite)(
                std::rc::Rc::clone(&requests),
                config.as_ref(),
                shared.clone(),
                std::rc::Rc::clone(&here),
                peer,
                &wake,
            )
        }))
    }

    pub(crate) fn workspace_settings(
        shared: crate::desktop::Shared,
        here: tilo_desktop::here::Here,
        peer: Option<u32>,
        records: Option<crate::workspace_settings::PageRecords>,
    ) -> Self {
        Self::new(Box::new(move |code| {
            (code.workspace_settings)(
                shared.clone(),
                std::rc::Rc::clone(&here),
                peer,
                records.clone(),
            )
        }))
    }

    fn new(make: Box<dyn Fn(SheetsCode) -> Box<dyn PageSheet>>) -> Self {
        Self {
            make,
            sheet: None,
            asked: std::cell::Cell::new(false),
        }
    }

    /// Open the sheet: at once when it is made, else as the page next
    /// follows it, once its code is in.
    pub(crate) fn open(&self) {
        self.asked.set(true);
        self.open_asked();
    }

    fn open_asked(&self) {
        if let Some(sheet) = self.sheet.as_ref().filter(|_| self.asked.get()) {
            sheet.open();
            self.asked.set(false);
        }
    }

    /// Make the sheet once its code is in and the person asked for it.
    fn take_up(&mut self) {
        if self.sheet.is_none()
            && self.asked.get()
            && let Some(code) = SHEETS.get()
        {
            self.sheet = Some((self.make)(code));
        }
        self.open_asked();
    }

    pub(crate) fn is_open(&self) -> bool {
        self.sheet.as_ref().is_some_and(|sheet| sheet.is_open())
    }
}

#[cfg(target_arch = "wasm32")]
impl OverSheet for LazySheet {
    fn follow_sheet(&mut self, show: &mut dyn FnMut(Option<Element>), missing: bool) -> bool {
        self.take_up();
        self.sheet
            .as_mut()
            .is_some_and(|sheet| sheet.follow_sheet(show, missing))
    }

    fn put_away(&self) {
        if let Some(sheet) = self.sheet.as_ref() {
            sheet.put_away();
        }
    }

    fn drawn(&self) -> bool {
        self.sheet.as_ref().is_some_and(|sheet| sheet.drawn())
    }

    fn sheet_name(&self) -> &'static str {
        self.sheet
            .as_ref()
            .map_or("sheet", |sheet| sheet.sheet_name())
    }
}

/// The sheet an agent's question is drawn on, made once the questions' code
/// is in: until then the page shows no question, because nothing has asked it
/// one ([`crate::questions`]).
#[cfg(target_arch = "wasm32")]
pub(crate) struct LazyQuestions {
    sheet: Option<Box<dyn OverSheet>>,
}

#[cfg(target_arch = "wasm32")]
impl LazyQuestions {
    pub(crate) fn new() -> Self {
        Self { sheet: None }
    }
}

#[cfg(target_arch = "wasm32")]
impl OverSheet for LazyQuestions {
    fn follow_sheet(&mut self, show: &mut dyn FnMut(Option<Element>), missing: bool) -> bool {
        if self.sheet.is_none()
            && crate::questions::answering()
            && let Some(code) = crate::questions::QUESTIONS.get()
        {
            self.sheet = Some((code.sheet)());
        }
        self.sheet
            .as_mut()
            .is_some_and(|sheet| sheet.follow_sheet(show, missing))
    }

    fn put_away(&self) {
        if let Some(sheet) = self.sheet.as_ref() {
            sheet.put_away();
        }
    }

    fn drawn(&self) -> bool {
        self.sheet.as_ref().is_some_and(|sheet| sheet.drawn())
    }

    fn sheet_name(&self) -> &'static str {
        self.sheet
            .as_ref()
            .map_or("questions", |sheet| sheet.sheet_name())
    }
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
        // The page put the sheet away: its card is off the page, so its leaving is over. Told
        // only when nothing of it is shown, a sheet left where it stood stays open.
        if content.is_none() {
            sheet.put_away();
        }
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
    /// The sheet it holds, over the whole page.
    fn html(&self, cx: &mut ui::composition::HtmlCx<'_>) {
        cx.element("div");
        cx.children();
    }

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
            content.draw_child(scene, transform, text, theme, presence);
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
        // Nothing shown is nothing trapped: a sheet the page put away leaves the keyboard to the
        // page at once, rather than holding every key on a card the page no longer draws.
        self.content.is_some() && self.sheet.drawn()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_sheets::{Phase, Presentation};

    /// A sheet whose card the page shows while `shown`, over the session's own presentation.
    struct Stub {
        presentation: Presentation,
        shown: std::cell::Cell<bool>,
    }

    impl Stub {
        fn new() -> Self {
            Self {
                presentation: Presentation::default(),
                shown: std::cell::Cell::new(false),
            }
        }
    }

    impl OverSheet for Stub {
        fn follow_sheet(&mut self, show: &mut dyn FnMut(Option<Element>), _missing: bool) -> bool {
            if self.shown.get() {
                show(Some(Box::new(ui::widget::Spacer::new())));
            } else {
                show(None);
            }
            false
        }

        fn put_away(&self) {
            if !self.presentation.is_open() {
                self.presentation.gone();
            }
        }

        fn drawn(&self) -> bool {
            self.presentation.is_drawn()
        }

        fn sheet_name(&self) -> &'static str {
            "stub"
        }
    }

    /// A sheet the page put away holds no key: the page's keyboard is its own again the moment
    /// nothing of the sheet is drawn, and the sheet's own leaving ends with it. A sheet whose
    /// card stands still traps the keyboard, as it takes what it covers.
    #[test]
    fn a_sheet_the_page_put_away_traps_nothing() {
        let mut page = OverPage::new(Stub::new());
        page.sheet.presentation.open(false);
        page.sheet.shown.set(true);
        page.follow();
        assert!(page.content.is_some(), "the card is on the page");
        assert!(Widget::traps_focus(&page), "a card up takes the keyboard");
        assert!(page.sheet.presentation.is_open());

        // Closed: the card leaves the page at once, however far its own exit has got.
        page.sheet.presentation.close();
        assert_eq!(page.sheet.presentation.phase(), Phase::Leaving);
        page.sheet.shown.set(false);
        page.follow();
        assert!(page.content.is_none(), "the page draws nothing of it");
        assert!(
            !Widget::traps_focus(&page),
            "a sheet not drawn traps nothing"
        );
        assert_eq!(
            page.sheet.presentation.phase(),
            Phase::Closed,
            "the page's putting it away ends its leaving"
        );
    }
}
