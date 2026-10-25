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

use std::collections::{HashMap, HashSet};

use ui::kurbo::{Affine, Point, Rect, Vec2};
use ui::taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use ui::time::{Duration, Instant};
use ui::vello::Scene;
use ui::widget::{TileContent, TileHost, Widget};
use ui::window_share::{self, EmbedState, OpenWindow, ResourceView};
use ui::window_toolbar::ToolbarAction;
use ui::{TextRenderer, Theme};

use super::DesktopSurface;
use crate::navigation::Admitted;

/// The size an embedded view is laid out at before it is scaled into its
/// place: about a window's, so the app lays it out as its sharer sees it.
const VIEW: (f64, f64) = (1000.0, 625.0);
/// How soon a view that could not show yet (its record still on its way)
/// is tried again.
const RETRY: Duration = Duration::from_secs(2);

const SHARE_ITEM: &str = "tilo.desktop.share-to-channel";
const STOP_ITEM: &str = "tilo.desktop.stop-sharing";

/// The views this desktop runs for the windows that embed them, and the
/// windows its menus offer to share.
#[derive(Default)]
pub(super) struct LiveShares {
    views: HashMap<ResourceView, View>,
    /// Where each view is drawn this frame, in the surface's coordinates:
    /// its place and what of it may show.
    placed: Vec<(ResourceView, Rect, Rect)>,
    /// The windows a share item stands in the menu of.
    offered: HashSet<u64>,
    /// The windows on a record, as last listed.
    windows: Vec<OpenWindow>,
}

/// One embedded view: its app, or why it cannot show.
struct View {
    host: Result<TileHost, String>,
    tried: Instant,
}

impl View {
    fn state(&self) -> EmbedState {
        match &self.host {
            Err(reason) => EmbedState::Unavailable(reason.clone()),
            Ok(host) => match host.content() {
                TileContent::App(_) => EmbedState::Showing,
                TileContent::Notice { detail, .. } => EmbedState::Unavailable(detail.clone()),
                _ => EmbedState::Loading,
            },
        }
    }

    fn settled(&self) -> bool {
        matches!(self.state(), EmbedState::Showing)
    }

    /// When it is tried again, while it does not show.
    fn retry_at(&self) -> Option<Instant> {
        self.tried.checked_add(RETRY)
    }
}

/// `rect` moved `dx` to the right.
fn shifted(rect: Rect, dx: f64) -> Rect {
    Rect::new(rect.x0 + dx, rect.y0, rect.x1 + dx, rect.y1)
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
                    let title = self
                        .window_slots
                        .toolbar(tab.id)
                        .and_then(|toolbar| toolbar.location.as_ref())
                        .map(|location| location.label.clone())
                        .filter(|label| !label.trim().is_empty())
                        .unwrap_or_else(|| tab.title.clone());
                    Some(OpenWindow {
                        tab: tab.id,
                        app: app.as_str().to_owned(),
                        app_title,
                        title,
                        record,
                        kind,
                    })
                })
                .filter(|window| window.app != window_share::SHARING_APP)
                .collect()
        };
        window_share::publish_windows(windows.clone());
        let shared = window_share::shared();
        let mut offered = HashSet::new();
        for window in &windows {
            let item = match shared
                .iter()
                .find(|share| share.app == window.app && share.record == window.record)
            {
                Some(share) => ToolbarAction::new(
                    STOP_ITEM,
                    "eye-off",
                    crate::i18n::t_args(
                        "window-stop-sharing-in",
                        &[("place", (&share.place).into())],
                    ),
                ),
                None => ToolbarAction::new(
                    SHARE_ITEM,
                    "screen-share",
                    crate::i18n::t("window-share-to-channel"),
                ),
            };
            self.window_slots.set_host_items(window.tab, vec![item]);
            offered.insert(window.tab);
        }
        for gone in self.live_shares.offered.difference(&offered) {
            self.window_slots.set_host_items(*gone, Vec::new());
        }
        self.live_shares.offered = offered;
        self.live_shares.windows = windows;
    }

    /// Hand the sharing app what the windows' menus asked of it. Whether
    /// anything was asked.
    pub(super) fn take_window_share_asks(&mut self) -> bool {
        let chosen = self.window_slots.take_host_actions();
        let mut asked = false;
        for (tab, item) in chosen {
            let Some(window) = self
                .live_shares
                .windows
                .iter()
                .find(|window| window.tab == tab)
            else {
                continue;
            };
            if item == SHARE_ITEM {
                window_share::ask_to_share(window.clone());
                // The sharing app asks where, in front.
                self.shared
                    .borrow_mut()
                    .queue
                    .push(Admitted::Open(window_share::SHARING_APP.to_owned()));
                asked = true;
            } else if item == STOP_ITEM {
                for share in window_share::shared()
                    .into_iter()
                    .filter(|share| share.app == window.app && share.record == window.record)
                {
                    window_share::ask_to_stop(share.message);
                    asked = true;
                }
            }
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
        let now = ui::time::now();
        let mut placed = Vec::new();
        let mut asking = Vec::new();
        if let Some(row) = self.shown.and_then(|strip| self.rows.get_mut(&strip)) {
            for group in &mut row.groups {
                let tab = group.active_tab_id();
                let Some(content) = group.content_rect_for_tab(tab) else {
                    continue;
                };
                let Some(host) = group.host_for_tab_mut(tab) else {
                    continue;
                };
                let shift = host.content_shift();
                let Some(window) = host.hosted_window().cloned() else {
                    continue;
                };
                for embed in window.embeds() {
                    let clip = shifted(embed.clip, shift).intersect(content);
                    placed.push((embed.view.clone(), shifted(embed.rect, shift), clip));
                    asking.push((window.clone(), embed.key, embed.view));
                }
            }
        }
        let mut changed = false;
        let wanted: HashSet<&ResourceView> = placed.iter().map(|(view, ..)| view).collect();
        let before = self.live_shares.views.len();
        self.live_shares
            .views
            .retain(|view, _| wanted.contains(view));
        changed |= self.live_shares.views.len() != before;
        for view in wanted {
            let retry = self.live_shares.views.get(view).is_none_or(|known| {
                !known.settled() && known.retry_at().is_some_and(|at| now >= at)
            });
            if retry {
                let host = self.tiles.embed(view);
                self.live_shares
                    .views
                    .insert(view.clone(), View { host, tried: now });
                changed = true;
            }
        }
        for (window, key, view) in asking {
            if let Some(known) = self.live_shares.views.get(&view) {
                window.set_embed_state(&key, known.state());
            }
        }
        for known in self.live_shares.views.values_mut() {
            if let Ok(host) = &mut known.host {
                host.sync(state, path);
                changed |= host.update(state, path);
            }
        }
        self.live_shares.placed = placed;
        changed
    }

    /// The soonest a view that could not show is tried again.
    pub(super) fn embed_retry_deadline(&self) -> Option<Instant> {
        self.live_shares
            .views
            .values()
            .filter(|view| !view.settled())
            .filter_map(View::retry_at)
            .min()
    }

    /// Whether a running view is animating.
    pub(super) fn embeds_animate(&self) -> bool {
        self.live_shares
            .views
            .values()
            .filter_map(|view| view.host.as_ref().ok())
            .any(Widget::needs_animation_frame)
    }

    /// What the running views wrote this frame goes to their records.
    pub(super) fn settle_embeds(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) {
        for view in self.live_shares.views.values_mut() {
            if let Ok(host) = &mut view.host {
                host.settle(edits, path);
            }
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
            let Some(Ok(host)) = self
                .live_shares
                .views
                .get_mut(view)
                .map(|view| view.host.as_mut())
            else {
                continue;
            };
            if !lay_out(host, theme, text) {
                continue;
            }
            let scale = (rect.width() / VIEW.0).min(rect.height() / VIEW.1);
            let place = transform
                .pre_translate(Vec2::new(rect.x0, rect.y0))
                .pre_scale(scale);
            scene.push_layer(
                ui::vello::peniko::Fill::NonZero,
                ui::vello::peniko::BlendMode::default(),
                1.0,
                transform,
                clip,
            );
            let outer = presence.push_viewport(Rect::new(0.0, 0.0, VIEW.0, VIEW.1));
            host.draw(scene, place, text, theme, presence);
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
