//! The display projection sheet.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Wireless projection is an OS-owned edge sheet. Discovery and
    /// protocol handshakes complete off-thread, and the host update moves
    /// their results and the display state into the sheet; painting only
    /// draws it. Split out of `draw_dropdowns_and_sheets`.
    pub(super) fn draw_projection_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) {
        self.projection_overlay.draw(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            self.rect,
            flags.projection,
        );
    }
}

impl ShellOverlayHost {
    /// Route the modal component before application content and apply its
    /// visibility/invalidation requests to global chrome state.
    pub(super) fn handle_projection_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let open = self.overlay_flags().projection;
        let outcome = self.projection_overlay.handle_event(
            event,
            ctx,
            self.last_cursor,
            self.overlay_area(),
            open,
        );
        if outcome.close_requested
            && let Some(chrome) = self.chrome.as_ref()
        {
            chrome.borrow_mut().close_projection();
        } else if outcome.consumed
            && is_press(event)
            && let Some(chrome) = self.chrome.as_ref()
        {
            // Once the person presses a key or a button in the sheet, it is
            // theirs: it no longer closes when the change it followed settles.
            chrome.borrow_mut().projection_follow =
                crate::shell::chrome_state::ProjectionFollow::Person;
        }
        if let Some(request) = self.projection_overlay.take_request() {
            self.invoke_display_request(request, ctx);
        }
        if outcome.invalidate
            && let Some(chrome) = self.chrome.as_ref()
        {
            chrome.borrow_mut().bump();
        }
        outcome.consumed
    }

    /// Make a display choice from the sheet through its published action,
    /// resolved and invoked exactly as the control socket does, so the
    /// person at the sheet and an agent pass one authority check and stage
    /// through one door. The sheet follows the job to its receipt.
    pub(super) fn invoke_display_request(
        &mut self,
        request: crate::shell::projection_overlay::DisplayRequest,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        use crate::shell::search::action_api::{ActionError, AllVisible, ResourceRef};
        let outcome = self
            .chrome
            .clone()
            .ok_or_else(|| ActionError::Refused("this shell has no desktop to act on".into()))
            .and_then(|chrome| {
                let resource = ResourceRef::new(
                    crate::shell::search::providers::actions::PROVIDER_ID,
                    request.action_id(),
                );
                // Visibility is not authority: the action itself checks the
                // workspace's grant when it is invoked, as for every caller.
                let catalog = self.action_catalog_for_resource(
                    &resource,
                    &chrome.borrow(),
                    ctx.state.doc(),
                    ctx.path,
                    &AllVisible,
                );
                let selected = catalog.select(&resource).ok_or_else(|| {
                    ActionError::Refused(format!("{} is not published", request.action_id()))
                })?;
                selected.invoke_nullary(&mut ctx.state, ctx.path, &chrome)
            });
        self.projection_overlay.follow_request(request, outcome);
    }

    /// Close the display sheet a `display.layout.*` action opened once the
    /// change it shows is kept, reverted, refused or replaced, so an agent
    /// or script that answered it leaves no modal sheet holding the
    /// keyboard. A sheet the person opened or used is left alone. Returns
    /// whether the sheet closed.
    pub(super) fn close_settled_projection(&mut self) -> bool {
        use crate::shell::chrome_state::ProjectionFollow;
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let (open, follow) = {
            let state = chrome.borrow();
            (state.overlays.projection, state.projection_follow)
        };
        match follow {
            ProjectionFollow::Transaction(id) if open => {
                if !self
                    .projection_overlay
                    .followed_change_settled(id, Instant::now())
                {
                    return false;
                }
                let mut state = chrome.borrow_mut();
                state.close_projection();
                state.bump();
                true
            }
            ProjectionFollow::Person => false,
            // Closed some other way: nothing is followed any more.
            _ if !open => {
                chrome.borrow_mut().projection_follow = ProjectionFollow::Person;
                false
            }
            _ => false,
        }
    }
}
