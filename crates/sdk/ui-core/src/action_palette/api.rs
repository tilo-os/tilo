//! The action palette as the windows that own one hold it: a trait object
//! made from [`ACTION_PALETTE`], whose code a program may fetch on first
//! use (`lazy-palettes`).

#![allow(
    clippy::wildcard_imports,
    reason = "the trait forwards the palette's own types"
)]

use super::*;

/// What the windows that own an action palette ask of it, as the trait
/// object [`ACTION_PALETTE`] makes.
pub(crate) trait ActionPaletteApi: crate::widget::Widget {
    #[cfg(test)]
    fn scope(&self) -> Option<&ScopeDecl>;
    #[cfg(test)]
    fn query(&self) -> &str;
    #[cfg(test)]
    fn result_labels(&self) -> Vec<String>;
    #[cfg(test)]
    fn in_argument_mode(&self) -> bool;
    #[cfg(test)]
    fn scrim_rect(&self) -> Option<Rect>;
    fn asking_why(&self) -> Option<&str>;
    fn result_rows(&self) -> Vec<(String, Option<String>)>;
    fn asking(&self) -> Option<(&tilo_actions::ActionAddress, &str)>;
    fn confirming_target(&self) -> Option<String>;
    fn load_open_resources(&mut self, resources: &[ResourceSpec]);
    fn refresh_visible_resources(&mut self, resources: &[ResourceSpec]);
    fn set_providers(&mut self, providers: Vec<Box<dyn SearchProvider>>);
    fn is_open(&self) -> bool;
    fn activate_ordinary(&mut self, request: crate::bound_dispatch::OrdinaryConfirmation);
    fn anchor_confirmation(&mut self, anchor: Option<Rect>);
    fn confirmation_anchor(&self) -> Option<Rect>;
    fn went_ahead(&self) -> bool;
    fn ordinary_request(&self) -> Option<&crate::bound_dispatch::OrdinaryConfirmation>;
    fn is_presented(&self) -> bool;
    fn suspend_for_history(&mut self);
    fn history_has_custody(&self) -> bool;
    fn retain_history_custody(
        &mut self,
        target: crate::bound_dispatch::local_target::LocalResourceTarget,
        identity: Option<Result<crate::bound_dispatch::ControlIdentity, String>>,
        witness: Option<Result<crate::bound_dispatch::ControlWitness, String>>,
    );
    fn retain_history_refusal(&mut self, pending: PendingOperation, reason: String);
    fn resume_after_history(&mut self);
    fn load_bound_resources(
        &mut self,
        snapshots: Vec<crate::bound_dispatch::BoundResourceSnapshot>,
    );
    fn open(&mut self);
    fn open_on(&mut self, request: &crate::search::SearchRequest) -> bool;
    fn set_query(&mut self, q: &str);
    fn advance_search_page(&mut self) -> SearchAdvance;
    fn take_operation(&mut self) -> Option<PendingOperation>;
    fn take_scoped_navigation(
        &mut self,
    ) -> Option<(String, Option<crate::bound_dispatch::CallbackBinding>)>;
    fn layout_with_text(
        &mut self,
        theme: &Theme,
        origin: Point,
        width: f64,
        text: &mut TextRenderer,
    );
    fn rect(&self) -> Rect;
    fn presentation_widgets(&self) -> (&dyn Widget, &dyn Widget);
    fn perform_host_semantic(
        &mut self,
        results_owner: bool,
        target: Option<&str>,
        action: &crate::a11y::Action,
        doc: &mut crate::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool;
    fn handle_host_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut crate::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool;
    fn place_in(&mut self, window: Rect, theme: &Theme) -> (Point, f64);
}

impl ActionPaletteApi for ActionPalette {
    #[cfg(test)]
    fn scope(&self) -> Option<&ScopeDecl> {
        ActionPalette::scope(self)
    }
    #[cfg(test)]
    fn query(&self) -> &str {
        ActionPalette::query(self)
    }
    #[cfg(test)]
    fn result_labels(&self) -> Vec<String> {
        ActionPalette::result_labels(self)
    }
    #[cfg(test)]
    fn in_argument_mode(&self) -> bool {
        ActionPalette::in_argument_mode(self)
    }
    #[cfg(test)]
    fn scrim_rect(&self) -> Option<Rect> {
        ActionPalette::scrim_rect(self)
    }
    fn asking_why(&self) -> Option<&str> {
        ActionPalette::asking_why(self)
    }
    fn result_rows(&self) -> Vec<(String, Option<String>)> {
        ActionPalette::result_rows(self)
    }
    fn asking(&self) -> Option<(&tilo_actions::ActionAddress, &str)> {
        ActionPalette::asking(self)
    }
    fn confirming_target(&self) -> Option<String> {
        ActionPalette::confirming_target(self)
    }
    fn load_open_resources(&mut self, resources: &[ResourceSpec]) {
        ActionPalette::load_open_resources(self, resources)
    }
    fn refresh_visible_resources(&mut self, resources: &[ResourceSpec]) {
        ActionPalette::refresh_visible_resources(self, resources)
    }
    fn set_providers(&mut self, providers: Vec<Box<dyn SearchProvider>>) {
        ActionPalette::set_providers(self, providers)
    }
    fn is_open(&self) -> bool {
        ActionPalette::is_open(self)
    }
    fn activate_ordinary(&mut self, request: crate::bound_dispatch::OrdinaryConfirmation) {
        ActionPalette::activate_ordinary(self, request)
    }
    fn anchor_confirmation(&mut self, anchor: Option<Rect>) {
        ActionPalette::anchor_confirmation(self, anchor)
    }
    fn confirmation_anchor(&self) -> Option<Rect> {
        ActionPalette::confirmation_anchor(self)
    }
    fn went_ahead(&self) -> bool {
        ActionPalette::went_ahead(self)
    }
    fn ordinary_request(&self) -> Option<&crate::bound_dispatch::OrdinaryConfirmation> {
        ActionPalette::ordinary_request(self)
    }
    fn is_presented(&self) -> bool {
        ActionPalette::is_presented(self)
    }
    fn suspend_for_history(&mut self) {
        ActionPalette::suspend_for_history(self)
    }
    fn history_has_custody(&self) -> bool {
        ActionPalette::history_has_custody(self)
    }
    fn retain_history_custody(
        &mut self,
        target: crate::bound_dispatch::local_target::LocalResourceTarget,
        identity: Option<Result<crate::bound_dispatch::ControlIdentity, String>>,
        witness: Option<Result<crate::bound_dispatch::ControlWitness, String>>,
    ) {
        ActionPalette::retain_history_custody(self, target, identity, witness)
    }
    fn retain_history_refusal(&mut self, pending: PendingOperation, reason: String) {
        ActionPalette::retain_history_refusal(self, pending, reason)
    }
    fn resume_after_history(&mut self) {
        ActionPalette::resume_after_history(self)
    }
    fn load_bound_resources(
        &mut self,
        snapshots: Vec<crate::bound_dispatch::BoundResourceSnapshot>,
    ) {
        ActionPalette::load_bound_resources(self, snapshots)
    }
    fn open(&mut self) {
        ActionPalette::open(self)
    }
    fn open_on(&mut self, request: &crate::search::SearchRequest) -> bool {
        ActionPalette::open_on(self, request)
    }
    fn set_query(&mut self, q: &str) {
        ActionPalette::set_query(self, q)
    }
    fn advance_search_page(&mut self) -> SearchAdvance {
        ActionPalette::advance_search_page(self)
    }
    fn take_operation(&mut self) -> Option<PendingOperation> {
        ActionPalette::take_operation(self)
    }
    fn take_scoped_navigation(
        &mut self,
    ) -> Option<(String, Option<crate::bound_dispatch::CallbackBinding>)> {
        ActionPalette::take_scoped_navigation(self)
    }
    fn layout_with_text(
        &mut self,
        theme: &Theme,
        origin: Point,
        width: f64,
        text: &mut TextRenderer,
    ) {
        ActionPalette::layout_with_text(self, theme, origin, width, text)
    }
    fn rect(&self) -> Rect {
        ActionPalette::rect(self)
    }
    fn presentation_widgets(&self) -> (&dyn Widget, &dyn Widget) {
        ActionPalette::presentation_widgets(self)
    }
    fn perform_host_semantic(
        &mut self,
        results_owner: bool,
        target: Option<&str>,
        action: &crate::a11y::Action,
        doc: &mut crate::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool {
        ActionPalette::perform_host_semantic(self, results_owner, target, action, doc, root)
    }
    fn handle_host_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut crate::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool {
        ActionPalette::handle_host_event(self, event, scale, modifiers, doc, root)
    }
    fn place_in(&mut self, window: Rect, theme: &Theme) -> (Point, f64) {
        ActionPalette::place_in(self, window, theme)
    }
}

/// An action palette, as a window holds it.
pub(crate) type ActionPaletteBox = Box<dyn ActionPaletteApi>;

/// The action palette's code, as a program that fetches it hands it over:
/// it makes a palette for the host text's language.
#[derive(Clone, Copy)]
pub struct MakeActionPalette(fn(String) -> ActionPaletteBox);

/// The action palette this build links.
#[must_use]
pub fn builtin_action_palette() -> MakeActionPalette {
    MakeActionPalette(|locale| Box::new(ActionPalette::with_host_text(locale)))
}

crate::lazy_code! {
    /// The action palette's code: linked, or with `lazy-palettes` fetched
    /// once a window first lays out, ahead of the first prompt or search it
    /// shows.
    pub static ACTION_PALETTE: MakeActionPalette = builtin_action_palette, feature = "lazy-palettes";
}

/// A palette for the host text's `locale`, or `None` while the palette's
/// code is still on its way (and then fetched).
pub(crate) fn make_action_palette(locale: String) -> Option<ActionPaletteBox> {
    ACTION_PALETTE.get().map(|make| (make.0)(locale))
}
