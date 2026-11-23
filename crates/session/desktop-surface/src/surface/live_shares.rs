//! Windows shared live into a conversation ([`ui::window_share`]), as this
//! desktop serves them.
//!
//! - **Which windows there are.** Every window on a record is listed for
//!   the sharing app's picker, and its menu offers "Share to channel…", or
//!   "Stop sharing" once it is shared; either reaches the sharing app as an
//!   ask, and sharing brings it forward to ask where.
//! - **The live views.** A window that embeds another app's view of a
//!   resource (a shared window's message) gets that app, run here on this
//!   desktop's own copy of the record. A miniature is laid out at a window's
//!   size and drawn scaled into the place the window gave it, and takes no
//!   input. A view the window asks for to work in stands in the desktop's own
//!   tree at its place's size, and takes the person's input there as a window
//!   does.
//!
//! The menus, the asks, when a view starts, retries and ends, and how a view
//! is laid out, drawn and handed input are every desktop's
//! ([`ui::window_share::host`]); this desktop says which windows ask and
//! starts each view's app.

use std::collections::HashSet;

use ui::kurbo::{Affine, Vec2};
use ui::time::Instant;
use ui::vello::Scene;
use ui::window_share::host::{self, Asking, Stage};
use ui::window_share::{self, Ask, OpenWindow};
use ui::{TextRenderer, Theme};

use super::DesktopSurface;
use tilo_session_core::desktop::navigation::Admitted;

/// The views this desktop runs for the windows that embed them, and the windows its menus offer to
/// share.
#[derive(Default)]
pub(super) struct LiveShares {
    pub(super) stage: Stage,
    /// The windows a share item stands in the menu of.
    offered: HashSet<u64>,
    /// The windows on a record, as last listed.
    windows: Vec<OpenWindow>,
}

impl DesktopSurface {
    /// List the windows on a record and put the share items in their menus,
    /// before the bars take the frame's slots.
    pub(super) fn offer_window_shares(&mut self) {
        if !self.tiles.draws_embeds() {
            // Nothing is shared from a desktop that cannot show the shares' views: no window offers
            // the sharing row.
            self.shared.borrow_mut().window_offers.clear();
            return;
        }
        window_share::set_drawn(true);
        let windows: Vec<OpenWindow> = {
            let shared = self.shared.borrow();
            shared
                .tree
                .tabs()
                .filter_map(|tab| {
                    let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
                    let (resource, kind) = self.tiles.window_resource(tab.id)?;
                    let app_title = match &tab.tile {
                        tilo_desktop_model::TileContent::LocalApp { label, .. } => label.clone(),
                        _ => tab.app_title.clone().unwrap_or_else(|| tab.title.clone()),
                    };
                    // What the window says it shows (a note's title) names the share better than
                    // its tab, which may name the app.
                    let location = self
                        .window_parts
                        .toolbar(tab.id)
                        .and_then(|toolbar| toolbar.location.as_ref());
                    let title = location
                        .map(|location| location.label.clone())
                        .filter(|label| !label.trim().is_empty())
                        .unwrap_or_else(|| tab.title.clone());
                    // Where in the record the window is: the note, not the notebook.
                    let item = location.and_then(|location| location.anchor.clone());
                    Some(OpenWindow {
                        tab: tab.id,
                        app: app.as_str().to_owned(),
                        app_title,
                        title,
                        record: resource,
                        kind,
                        item,
                    })
                })
                .filter(|window| window.app != window_share::SHARING_APP)
                .collect()
        };
        window_share::publish_windows(windows.clone());
        host::offer(
            &mut self.window_parts,
            &windows,
            &window_share::shared(),
            &mut self.live_shares.offered,
        );
        // A window's own menu offers the same share row its bar does.
        self.shared.borrow_mut().window_offers.clone_from(&windows);
        self.live_shares.windows = windows;
    }

    /// Ask the sharing app for window `tab` 's share row: share it with its workspace's people, or
    /// stop sharing it. Whether it was asked: false when the app lists no such window, or already
    /// does what is asked.
    pub(super) fn ask_window_share(&self, tab: tilo_desktop_model::TabId, share: bool) -> bool {
        let shared = window_share::shared();
        let Some(window) = self
            .live_shares
            .windows
            .iter()
            .find(|window| window.tab == tab)
            .cloned()
        else {
            return false;
        };
        let wanted = if share {
            host::SHARE_ITEM
        } else {
            host::STOP_ITEM
        };
        if host::menu_item(&window, &shared).id != wanted {
            // Already shared, or already not.
            return false;
        }
        let asks = host::asks(
            vec![(tab, wanted.to_owned())],
            &self.live_shares.windows,
            &shared,
        );
        let asked = !asks.is_empty();
        for ask in asks {
            if matches!(ask, Ask::Share(_)) {
                // The sharing app asks where, in front.
                self.shared
                    .borrow_mut()
                    .queue
                    .push(Admitted::Open(window_share::SHARING_APP.to_owned()));
            }
            window_share::deliver_ask(ask);
        }
        asked
    }

    /// Hand the sharing app what the windows' menus asked of it. Whether anything was asked.
    pub(super) fn take_window_share_asks(&mut self) -> bool {
        let chosen = self.window_parts.take_host_actions();
        let asks = host::asks(chosen, &self.live_shares.windows, &window_share::shared());
        let asked = !asks.is_empty();
        for ask in asks {
            if matches!(ask, Ask::Share(_)) {
                // The sharing app asks where, in front.
                self.shared
                    .borrow_mut()
                    .queue
                    .push(Admitted::Open(window_share::SHARING_APP.to_owned()));
            }
            window_share::deliver_ask(ask);
        }
        asked
    }

    /// Run the views the shown windows embed: start the ones newly asked for, drop the ones no
    /// longer asked for, and tell each window how its views are coming along. Whether any view
    /// changed.
    pub(super) fn follow_embeds(
        &mut self,
        state: &ui::widget::AppModel,
        path: &ui::widget::ModelPath,
    ) -> bool {
        if !self.tiles.draws_embeds() {
            return false;
        }
        let mut asking = Vec::new();
        if let Some(row) = self.shown.and_then(|strip| self.rows.get_mut(&strip)) {
            for group in &mut row.groups {
                let tab = group.active_tab_id();
                let Some(content) = group.content_rect_for_tab(tab) else {
                    continue;
                };
                let Some(tile) = group.host_for_tab_mut(tab) else {
                    continue;
                };
                let origin = Vec2::new(tile.content_shift(), 0.0);
                let Some(window) = tile.hosted_window().cloned() else {
                    continue;
                };
                asking.push(Asking::new(window, origin, content));
            }
        }
        let tiles = &mut self.tiles;
        let embedded = &mut self.live_shares.stage;
        embedded.set_workspace(tiles.workspace_id());
        embedded.follow(
            asking,
            ui::time::now(),
            |view| tiles.embed(view),
            state,
            path,
        )
    }

    /// The soonest a view that could not show is tried again.
    pub(super) fn embed_retry_deadline(&self) -> Option<Instant> {
        self.live_shares.stage.retry_deadline()
    }

    /// Whether a running view is animating.
    pub(super) fn embeds_animate(&self) -> bool {
        self.live_shares.stage.animating()
    }

    /// What the miniatures wrote this frame goes to their records; a view worked in settles with
    /// the windows.
    pub(super) fn settle_embeds(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) {
        self.live_shares.stage.settle(edits, path);
    }

    /// Draw each view over the window that embeds it, clipped to what of its place may show.
    pub(super) fn draw_embeds(&mut self, ctx: &mut tilo_ui::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.live_shares
            .stage
            .draw(&mut tilo_ui::DrawCtx::new(scene, transform, text, theme, env.clone()));
    }
}

#[cfg(test)]
#[path = "live_shares_tests.rs"]
mod tests;
