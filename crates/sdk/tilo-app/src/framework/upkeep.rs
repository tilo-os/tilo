//! What a view keeps up beside its widgets, that no widget of its own
//! does: the work it takes in on every sync of its window (background
//! reads landing, a request another part made), whether it still owes the
//! window frames or a wake, whether a capture waits for it, what it asks of
//! the host (the look the window shows itself in, the collaboration its
//! host actions share), and the presentation it keeps with the window
//! across a reopen. A view says it with
//! [`ViewCx::upkeep`](super::ViewCx::upkeep) around what it lays out, so it
//! never needs a widget of its own to do it.

use ui_core::application_view::{
    ApplicationFocusRestore, ApplicationLocalView, ApplicationViewState,
};
use ui_core::collaboration::{CollabEvent, CollabRequest};
use ui_core::presence::PresenceCtx;
use ui_core::text_render::TextRenderer;
use ui_core::theme::{AppearanceIntent, Theme};
use ui_core::time::Instant;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::Affine;
use ui_core::view::ThawError;
use ui_core::widget::{AppModel, Collaborative, Decorator, Element, ModelPath, Widget};

type Hook = Box<dyn FnMut()>;
type Ask = Box<dyn Fn() -> bool>;
type Wake = Box<dyn Fn() -> Option<Instant>>;
type Appearance = Box<dyn Fn() -> Option<AppearanceIntent>>;
type TakeRequests = Box<dyn FnMut() -> Vec<CollabRequest>>;
type HandleEvent = Box<dyn FnMut(&CollabEvent)>;
type Restore = Box<dyn FnMut(&[u8]) -> Result<(), String>>;

/// The presentation a view keeps with its window ([`Upkeep::kept_view`]).
struct KeptView {
    app_id: String,
    version: u32,
    capture: Box<dyn Fn() -> Vec<u8>>,
    restore: Restore,
}

/// What a view keeps up beside its widgets. See the [module docs](self).
/// Each source is asked whenever the window asks its widgets the same, so it
/// reads the view's state as it is then.
#[derive(Default)]
pub struct Upkeep {
    sync: Option<Hook>,
    busy: Option<Ask>,
    next_wake: Option<Wake>,
    pending: Option<Ask>,
    presented: Option<Hook>,
    appearance: Option<Appearance>,
    collaboration: Option<(TakeRequests, HandleEvent)>,
    view: Option<KeptView>,
}

impl Upkeep {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `sync` on every sync of the window, before the view's widgets
    /// sync: take in what work finished, move the view's state, so the
    /// parts keyed by it are built again in the same frame.
    #[must_use]
    pub fn syncing(mut self, sync: impl FnMut() + 'static) -> Self {
        self.sync = Some(Box::new(sync));
        self
    }

    /// Whether work the view runs is under way that the window keeps
    /// drawing frames for while it lasts.
    #[must_use]
    pub fn busy(mut self, busy: impl Fn() -> bool + 'static) -> Self {
        self.busy = Some(Box::new(busy));
        self
    }

    /// When the view next wants a sync while nothing else happens (a read
    /// on a timer, a notice that expires).
    #[must_use]
    pub fn next_wake(mut self, next: impl Fn() -> Option<Instant> + 'static) -> Self {
        self.next_wake = Some(Box::new(next));
        self
    }

    /// Whether what the view shows is still on its way (a first reading):
    /// a still capture waits for it, and a live window asks no frames on
    /// its account.
    #[must_use]
    pub fn pending_visual_work(mut self, pending: impl Fn() -> bool + 'static) -> Self {
        self.pending = Some(Box::new(pending));
        self
    }

    /// Run `presented` after each frame the view draws.
    #[must_use]
    pub fn presenting(mut self, presented: impl FnMut() + 'static) -> Self {
        self.presented = Some(Box::new(presented));
        self
    }

    /// The look the window shows itself in while `appearance` says one (a
    /// page that changes the look shows it the instant it changes); the
    /// host applies what differs.
    #[must_use]
    pub fn appearance(
        mut self,
        appearance: impl Fn() -> Option<AppearanceIntent> + 'static,
    ) -> Self {
        self.appearance = Some(Box::new(appearance));
        self
    }

    /// Share the app's collaboration connection with the view's host
    /// actions: `take` drains what they ask the collaboration host, and
    /// `handle` hands them each event it answers with.
    #[must_use]
    pub fn collaboration(
        mut self,
        take: impl FnMut() -> Vec<CollabRequest> + 'static,
        handle: impl FnMut(&CollabEvent) + 'static,
    ) -> Self {
        self.collaboration = Some((Box::new(take), Box::new(handle)));
        self
    }

    /// Keep what `capture` says of the view's presentation (the page it
    /// shows) with the window, as the app `app_id` at `version`: a reopened
    /// window hands it to `restore`, which refuses what it cannot show.
    #[must_use]
    pub fn kept_view(
        mut self,
        app_id: impl Into<String>,
        version: u32,
        capture: impl Fn() -> Vec<u8> + 'static,
        restore: impl FnMut(&[u8]) -> Result<(), String> + 'static,
    ) -> Self {
        self.view = Some(KeptView {
            app_id: app_id.into(),
            version,
            capture: Box::new(capture),
            restore: Box::new(restore),
        });
        self
    }
}

/// `content`, kept up as `upkeep` says.
pub(crate) fn upkeep(upkeep: Upkeep, content: Element) -> Element {
    Box::new(Kept { upkeep, content })
}

struct Kept {
    upkeep: Upkeep,
    content: Element,
}

impl Decorator for Kept {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn provide<'a>(&'a self, request: &mut ui_core::widget::capability::Request<'a, '_>) {
        if self.upkeep.view.is_some() && request.wants::<dyn ApplicationViewState>() {
            request.provide::<dyn ApplicationViewState>(self);
            return;
        }
        self.content.provide(request);
    }

    fn provide_mut<'a>(
        &'a mut self,
        request: &mut ui_core::widget::capability::RequestMut<'a, '_>,
    ) {
        if self.upkeep.collaboration.is_some() && request.wants::<dyn Collaborative>() {
            request.provide::<dyn Collaborative>(self);
            return;
        }
        if self.upkeep.view.is_some() && request.wants::<dyn ApplicationViewState>() {
            request.provide::<dyn ApplicationViewState>(self);
            return;
        }
        self.content.provide_mut(request);
    }

    // Whoever reaches the view's widget by its type finds it through the
    // part that keeps it up.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        self.content.as_any()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        self.content.as_any_mut()
    }

    fn name(&self) -> &'static str {
        "Upkeep"
    }

    // It adds no structure of its own: what the content changed is all.
    fn take_structure_change(&mut self) -> ui_core::widget::StructureChange {
        self.content.take_structure_change()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(sync) = self.upkeep.sync.as_mut() {
            sync();
        }
        self.content.sync(state, path);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
        if let Some(presented) = self.upkeep.presented.as_mut() {
            presented();
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.upkeep.busy.as_ref().is_some_and(|busy| busy()) || self.content.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let own = self.upkeep.next_wake.as_ref().and_then(|next| next());
        match (own, self.content.next_wake_deadline()) {
            (Some(own), Some(content)) => Some(own.min(content)),
            (own, content) => own.or(content),
        }
    }

    fn has_pending_visual_work(&self) -> bool {
        self.upkeep
            .pending
            .as_ref()
            .is_some_and(|pending| pending())
            || self.content.has_pending_visual_work()
    }

    fn appearance_intent(&self) -> Option<AppearanceIntent> {
        self.upkeep
            .appearance
            .as_ref()
            .and_then(|appearance| appearance())
            .or_else(|| self.content.appearance_intent())
    }
}

impl Collaborative for Kept {
    fn take_collab_requests(&mut self) -> Vec<CollabRequest> {
        self.upkeep
            .collaboration
            .as_mut()
            .map_or_else(Vec::new, |(take, _)| take())
    }

    fn handle_collab_event(&mut self, event: &CollabEvent) {
        if let Some((_, handle)) = self.upkeep.collaboration.as_mut() {
            handle(event);
        }
    }
}

impl ApplicationViewState for Kept {
    fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError> {
        Ok(self.upkeep.view.as_ref().map(|view| ApplicationLocalView {
            app_id: view.app_id.clone(),
            version: view.version,
            payload: (view.capture)(),
        }))
    }

    fn restore_application_view(
        &mut self,
        local: &ApplicationLocalView,
        _doc: &ui_core::automerge::AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError> {
        let view = self
            .upkeep
            .view
            .as_mut()
            .ok_or_else(|| ThawError::State("no kept view".into()))?;
        if local.app_id != view.app_id || local.version != view.version {
            return Err(ThawError::State(format!(
                "a view of {} at version {}, not {} at {}",
                local.app_id, local.version, view.app_id, view.version
            )));
        }
        (view.restore)(&local.payload).map_err(ThawError::State)?;
        Ok(ApplicationFocusRestore::Generic)
    }
}

#[cfg(test)]
#[path = "upkeep_tests.rs"]
mod tests;
