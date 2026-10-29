//! The Music window as a bound view: the [`Music`] it shares with the
//! view's effect handler and what the view publishes, and [`MusicApp`], the
//! widget that hosts it.
//!
//! Effects and publications reach the [`Music`] between frames; the window
//! reaches it while it syncs, lays out, draws and takes input, lending it
//! the page's embedded widgets for that, which it holds as its children
//! otherwise.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::framework::{EffectAnswer, EffectCx, Live, Publishes, ViewCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::Invoke;
use ui::time::Instant;
use ui::widget::controls::{ActionControls, ControlResult};
use ui::widget::{AppModel, Continuable, Element, ModelPath, Queryable, TaffyTreeExt, Widget};

use super::{Music, Page};
use crate::actions::MusicEffect;
use crate::model::Library;

/// The [`Music`] as the window and the view's effect handler share it.
type Shared = Rc<RefCell<Music>>;

/// The window: the shared [`Music`], following the library the document
/// holds, with the page's embedded widgets as its children.
pub struct MusicApp {
    music: Shared,
    /// The page's embedded widgets, lent to [`Self::music`] while it draws,
    /// lays out, syncs or takes input.
    page: Page,
    /// The library as the document holds it, shown as it changes; `None`
    /// for a window outside the framework.
    live: Option<Live<Library>>,
    /// The `(revision, loads)` of the library last shown.
    presented: Option<(u64, u64)>,
    /// The capture scenario to stage once the library is shown.
    staged: Rc<RefCell<Option<String>>>,
    /// The tooltip under the pointer, as the page last said.
    tooltip: Option<String>,
    rect: Rect,
}

impl MusicApp {
    /// The window over `music`, holding the widgets embedded in its page.
    fn over(mut music: Music) -> Self {
        let page = std::mem::take(&mut music.page);
        Self {
            music: Rc::new(RefCell::new(music)),
            page,
            live: None,
            presented: None,
            staged: Rc::default(),
            tooltip: None,
            rect: Rect::ZERO,
        }
    }

    /// What the window shows, shared with the view's effect handler.
    #[cfg(test)]
    pub(crate) fn state(&self) -> std::cell::RefMut<'_, Music> {
        self.music.borrow_mut()
    }

    /// Run `step` on the [`Music`] with the page's widgets lent to it.
    fn lend<T>(&mut self, step: impl FnOnce(&mut Music) -> T) -> T {
        let mut music = self.music.borrow_mut();
        std::mem::swap(&mut music.page, &mut self.page);
        let out = step(&mut music);
        std::mem::swap(&mut music.page, &mut self.page);
        self.tooltip = Widget::tooltip(&*music).map(str::to_owned);
        out
    }
}

impl Music {
    /// Answer one of Music's effects, invoked by a control, the palette, an
    /// agent or a script.
    fn answer_effect(&mut self, effect: MusicEffect, cx: &mut EffectCx<Library>) -> EffectAnswer {
        // The song that plays is the one playing as the effect runs, not
        // the one a control saw when it was bound.
        let effect = match effect {
            MusicEffect::ToggleLikePlaying => match self.playing_track() {
                Some(track) => MusicEffect::ToggleLike { track },
                None => return Err("Nothing is playing".into()),
            },
            effect => effect,
        };
        if let MusicEffect::ToggleLike { track } = &effect {
            if cx.dispatcher().read_only() {
                return Err("Liking a song requires permission to edit the library".into());
            }
            let current = cx.live().get();
            let _gesture = cx.dispatcher().begin_group();
            if !current.tracks.contains_key(track) {
                cx.submit(self.catalogue_import(track)?);
            }
            cx.submit(
                Invoke::edit(&Library::like(track, !current.is_liked(track)))
                    .map_err(|error| error.to_string())?,
            );
            return Ok(ui::framework::Effected::done());
        }
        let invocation = cx.invocation().unwrap_or_default();
        let answer = self.begin_effect(invocation, effect)?;
        if matches!(answer, ui::framework::Effected::Pending) {
            self.await_effect(invocation, cx.reply());
        }
        Ok(answer)
    }
}

/// The view of the library: `music` takes part in the resource, answers
/// Music's effects and publishes this device's player, queue, open file
/// and offline songs as rows, the library's edits bound to them, and the
/// songs the live catalogue found, which it looks the platform search's
/// queries up in.
pub(crate) fn window(cx: &ViewCx<Library>, mut music: Music) -> Element {
    music.attach_dispatcher(cx.dispatcher());
    music.attach_actions(cx.actions());
    // The person's kept views follow them; the search wakes this window
    // when the catalogue answers.
    music.attach_views(cx.settings::<ui::framework::views::KeptViews>(), cx.waker());
    if cx.mode().is_live() {
        music.open_launch(&cx.launch());
    }
    let mut window = MusicApp::over(music);
    window.live = Some(cx.live().clone());
    let music = Rc::clone(&window.music);
    {
        let answer = Rc::clone(&music);
        cx.effects(move |effect: MusicEffect, cx| answer.borrow_mut().answer_effect(effect, cx));
    }
    {
        let staged = Rc::clone(&window.staged);
        cx.on_stage(move |scenario| {
            *staged.borrow_mut() = scenario.map(str::to_owned);
        });
    }
    let (rows, row, edits, entries) = (
        Rc::clone(&music),
        Rc::clone(&music),
        Rc::clone(&music),
        Rc::clone(&music),
    );
    cx.publishing(
        Publishes::new()
            // This device's player, queue and open file, and the tracks it
            // keeps offline: rows beside the library, carrying their effects.
            .live_rows(move || rows.borrow().device_rows())
            .live_row(move |id| row.borrow().device_row(id))
            // The library's field edits bound to the row they act on: saving
            // an album, following an artist, taking a song out of a playlist
            // or moving it, and the recommendations' switches and feedback.
            .row_edits(move |row| edits.borrow().row_edits(row))
            // The songs the live search found that the library does not hold.
            .search_entries(move || entries.borrow().live_search_entries())
            .search_query(move |query| music.borrow_mut().queue_catalogue_query(query.text())),
        Box::new(window),
    )
}

impl Widget for MusicApp {
    ui::provides!(dyn Continuable, dyn ActionControls, dyn Queryable);

    fn modal_capture(&self) -> Option<Rect> {
        self.music.borrow().modal_capture()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.music.borrow().wants_pointer_capture()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.lend(Widget::take_structure_dirty)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mode = self.music.borrow().source_widget_mode();
        self.page.children(mode)
    }

    fn lays_out_child(&self, child_index: usize) -> bool {
        child_index < self.page.painted()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mode = self.music.borrow().source_widget_mode();
        self.page.children_mut(mode)
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.lend(|music| music.draw_overlay(scene, transform, text_renderer, theme, presence));
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let library = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented));
        let staged = self.staged.borrow_mut().take();
        self.lend(|music| {
            if let Some(library) = library {
                music.adopt_resource_state(&library);
            }
            if let Some(scenario) = staged {
                music.stage_scenario(&scenario);
            }
            music.sync(state, path);
        });
    }

    fn settle(&mut self, state: &mut ui::widget::Edits<'_>, path: &ObjId) {
        self.lend(|music| music.settle(state, path));
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let children = self.lend(|music| music.layout_page(taffy, theme, node_map));
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        self.music.borrow_mut().apply_layout(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.lend(|music| music.draw(scene, transform, text_renderer, theme, presence));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.lend(|music| music.handle_event(event, ctx));
    }

    fn name(&self) -> &'static str {
        "MusicApp"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.music.borrow().accessibility()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.lend(|music| music.perform_semantic_action(target, action, state, path))
    }

    fn tooltip(&self) -> Option<&str> {
        self.tooltip.as_deref()
    }

    fn needs_animation_frame(&self) -> bool {
        self.music.borrow().needs_animation_frame() || self.page.rows_moving()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let music = self.music.borrow().next_wake_deadline();
        music
            .into_iter()
            .chain(
                self.children()
                    .into_iter()
                    .filter_map(Widget::next_wake_deadline),
            )
            .min()
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.music.borrow().primary_vertical_scroll()
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.music
            .borrow_mut()
            .apply_primary_vertical_scroll(action)
    }

    fn has_pending_visual_work(&self) -> bool {
        self.music.borrow().has_pending_visual_work()
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.lend(|music| music.update(state, path))
    }

    fn needs_redraw(&self) -> bool {
        self.music.borrow().needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.music.borrow_mut().mark_drawn();
    }
}

impl Continuable for MusicApp {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        path: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        self.music.borrow().continuation_metadata(state, path)
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        self.lend(|music| music.restore_continuation_metadata(metadata, state, path))
    }
}

impl Queryable for MusicApp {
    fn query_opened(&mut self) {
        self.lend(Queryable::query_opened);
    }

    fn set_query_text(&mut self, text: &str) {
        self.lend(|music| music.set_query_text(text));
    }

    fn enter_results(&mut self) {
        self.lend(Queryable::enter_results);
    }

    fn query_closed(&mut self) {
        self.lend(Queryable::query_closed);
    }
}

impl ActionControls for MusicApp {
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &ControlResult,
    ) {
        self.lend(|music| music.on_control_invocation_result(address, result));
    }

    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        self.lend(|music| music.install_control_custody(domain, attachment));
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.lend(ActionControls::take_control_invocation)
    }

    /// The page's own bindings, then its embedded widgets'.
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = self.music.borrow().control_action_bindings();
        for child in self.children() {
            bindings.extend(ui::widget::controls::control_action_bindings(child));
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.lend(|music| music.refresh_control_action_availability(allowed));
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.music.borrow().has_pending_control_invocation()
            || self
                .children()
                .into_iter()
                .any(ui::widget::controls::has_pending_control_invocation)
    }
}
