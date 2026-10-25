//! Windows shared live into a conversation ([`ui::window_share`]), as this
//! desktop serves them.
//!
//! - **Which windows there are.** Every window on a record is listed for
//!   the sharing app's picker, and its menu offers "Share to channel…", or
//!   "Stop sharing" once it is shared; either reaches the sharing app as an
//!   ask, and sharing brings it forward to ask where.
//! - **The live views.** A window that embeds another app's view of a
//!   resource (a shared window's message) gets that app, run here on this
//!   desktop's own copy of the record, laid out at a window's size and drawn
//!   scaled into the place the window gave it. It takes no input: opening
//!   the resource is the embedding app's own action.
//!
//! The menus, the asks and when a view starts, retries and ends are every
//! desktop's ([`ui::window_share::host`]); this desktop runs each view's app
//! beside its windows, as a [`TileHost`] it lays out and draws itself.

use std::collections::{HashMap, HashSet};

use ui::kurbo::{Affine, Point, Rect, Vec2};
use ui::taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use ui::time::Instant;
use ui::vello::Scene;
use ui::widget::{TileHost, Widget};
use ui::window_share::host::{self, VIEW, Views};
use ui::window_share::{self, Ask, OpenWindow, ResourceView};
use ui::{TextRenderer, Theme};

use super::DesktopSurface;
use crate::navigation::Admitted;

/// The views this desktop runs for the windows that embed them, and the
/// windows its menus offer to share.
#[derive(Default)]
pub(super) struct LiveShares {
    views: Views<TileHost>,
    /// Where each view is drawn this frame, in the surface's coordinates:
    /// its place and what of it may show.
    placed: Vec<(ResourceView, Rect, Rect)>,
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
            return;
        }
        window_share::set_drawn(true);
        let windows: Vec<OpenWindow> = {
            let shared = self.shared.borrow();
            shared
                .tree
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter_map(|tab| {
                    let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
                    let (record, kind) = self.tiles.window_record(tab.id)?;
                    let app_title = match &tab.tile {
                        tilo_desktop_model::TileContent::LocalApp { label, .. } => label.clone(),
                        _ => tab.app_title.clone().unwrap_or_else(|| tab.title.clone()),
                    };
                    // What the window says it shows (a note's title) names
                    // the share better than its tab, which may name the app.
                    let location = self
                        .window_slots
                        .toolbar(tab.id)
                        .and_then(|toolbar| toolbar.location.as_ref());
                    let title = location
                        .map(|location| location.label.clone())
                        .filter(|label| !label.trim().is_empty())
                        .unwrap_or_else(|| tab.title.clone());
                    // Where in the record the window is: the note, not the
                    // notebook.
                    let item = location.and_then(|location| location.anchor.clone());
                    Some(OpenWindow {
                        tab: tab.id,
                        app: app.as_str().to_owned(),
                        app_title,
                        title,
                        record,
                        kind,
                        item,
                    })
                })
                .filter(|window| window.app != window_share::SHARING_APP)
                .collect()
        };
        window_share::publish_windows(windows.clone());
        host::offer(
            &mut self.window_slots,
            &windows,
            &window_share::shared(),
            &mut self.live_shares.offered,
        );
        self.live_shares.windows = windows;
    }

    /// Hand the sharing app what the windows' menus asked of it. Whether
    /// anything was asked.
    pub(super) fn take_window_share_asks(&mut self) -> bool {
        let chosen = self.window_slots.take_host_actions();
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

    /// Run the views the shown windows embed: start the ones newly asked
    /// for, drop the ones no longer asked for, and tell each window how its
    /// views are coming along. Whether any view changed.
    pub(super) fn follow_embeds(
        &mut self,
        state: &ui::widget::AppModel,
        path: &ui::widget::ModelPath,
    ) -> bool {
        if !self.tiles.draws_embeds() {
            return false;
        }
        let mut placed = Vec::new();
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
                for embed in window.embeds() {
                    let (rect, clip) = host::placed(&embed, origin, content);
                    placed.push((embed.view.clone(), rect, clip));
                    asking.push((window.clone(), embed.key, embed.view));
                }
            }
        }
        let tiles = &mut self.tiles;
        let followed = self.live_shares.views.follow(
            placed.iter().map(|(view, ..)| view),
            ui::time::now(),
            |view| tiles.embed(view),
        );
        let mut changed = followed.changed;
        for (window, key, view) in asking {
            if let Some(state) = self.live_shares.views.state(&view) {
                window.set_embed_state(&key, state);
            }
        }
        for (_, host) in self.live_shares.views.running_mut() {
            host.sync(state, path);
            changed |= host.update(state, path);
        }
        self.live_shares.placed = placed;
        changed
    }

    /// The soonest a view that could not show is tried again.
    pub(super) fn embed_retry_deadline(&self) -> Option<Instant> {
        self.live_shares.views.retry_deadline()
    }

    /// Whether a running view is animating.
    pub(super) fn embeds_animate(&self) -> bool {
        self.live_shares
            .views
            .running()
            .any(|(_, host)| host.needs_animation_frame())
    }

    /// What the running views wrote this frame goes to their records.
    pub(super) fn settle_embeds(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) {
        for (_, host) in self.live_shares.views.running_mut() {
            host.settle(edits, path);
        }
    }

    /// Draw each view over the window that embeds it, laid out at a
    /// window's size and scaled into its place, clipped to what of it may
    /// show.
    pub(super) fn draw_embeds(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let placed = std::mem::take(&mut self.live_shares.placed);
        for (view, rect, clip) in &placed {
            if clip.is_zero_area() || rect.is_zero_area() {
                continue;
            }
            let Some(tile) = self.live_shares.views.get_mut(view) else {
                continue;
            };
            if !lay_out(tile, theme, text) {
                continue;
            }
            scene.push_layer(
                ui::vello::peniko::Fill::NonZero,
                ui::vello::peniko::BlendMode::default(),
                1.0,
                transform,
                clip,
            );
            let outer = presence.push_viewport(Rect::new(0.0, 0.0, VIEW.0, VIEW.1));
            tile.draw(
                scene,
                host::view_transform(transform, *rect),
                text,
                theme,
                presence,
            );
            presence.pop_viewport(outer);
            scene.pop_layer();
        }
        self.live_shares.placed = placed;
    }
}

/// Lay `host` out at [`VIEW`]'s size from the origin. Whether it could.
fn lay_out(host: &mut TileHost, theme: &Theme, text: &mut TextRenderer) -> bool {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, theme, &mut nodes);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the view's logical size is far inside f32's range"
    )]
    let size = Size {
        width: ui::taffy::Dimension::length(VIEW.0 as f32),
        height: ui::taffy::Dimension::length(VIEW.1 as f32),
    };
    let Ok(root) = taffy.new_with_children(
        Style {
            size,
            ..Style::default()
        },
        &[child],
    ) else {
        return false;
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the view's logical size is far inside f32's range"
    )]
    let available = Size {
        width: AvailableSpace::Definite(VIEW.0 as f32),
        height: AvailableSpace::Definite(VIEW.1 as f32),
    };
    let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
    // SAFETY: the arena and node map were just built from `host` alone,
    // which stays in place and unaliased for both calls.
    let solved = unsafe {
        bindings.rebuild(&*host, &taffy, child, &nodes);
        ui::widget::compute_widget_layout(&mut taffy, root, available, &nodes, &bindings, text)
    };
    if solved.is_err() {
        return false;
    }
    host.apply_layout(&taffy, child, Point::ORIGIN);
    true
}

#[cfg(test)]
#[path = "live_shares_tests.rs"]
mod tests;
