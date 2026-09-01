//! Binary shim for the qOS `store` — mirrors the sibling native apps.
//!
//! Two modes:
//!
//!   * default — build a [`store::StoreApp`], hand it to `ui::App`, run
//!     the live event loop. One serialized worker owns catalogue refreshes
//!     and package mutations; the app drains its snapshots inside `sync`.
//!   * `--shots <PATH>` — render one offscreen frame to `<PATH>` and exit.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use store::app::RemoteIntent;
use store::worker::{Request, StoreWorker};
use store::{FlatpakRef, StoreApp};
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};

fn main() {
    // rustls needs an explicit provider: the tree carries both ring and
    // aws-lc-rs, and 0.23 will not pick one on its own. The dependency
    // was already declared for this — the call was not there, so every
    // https registry and every Flathub request failed at the TLS
    // handshake rather than anywhere a reader would look. Same line the
    // maps app and `qos-catalogue` run.
    let _ = rustls::crypto::ring::default_provider().install_default();

    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("dev.qos.store", live_root)
            .max_positionals(1)
            .shot_handler(shot_handler),
    );
}

fn live_root(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    // Opening a reference only stages a regular Store card. Installation stays
    // gated on the card's explicit Install action.
    let opened_ref = args
        .positionals()
        .first()
        .map(std::path::Path::new)
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("flatpakref"))
        })
        .map(FlatpakRef::from_path)
        .transpose()
        .map_err(|error| LaunchError::Operation(error.to_string()))?
        .flatten();
    Ok(Box::new(LiveStore::new(StoreWorker::spawn(opened_ref))))
}

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| LaunchError::Operation("missing screenshot output".into()))?;
    store::shots::render_to_path(
        args.viewport
            .unwrap_or(store::shots::DEFAULT_SHOTS_VIEWPORT),
        out,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// The Widget the main event loop owns. Wraps a `StoreApp` and
/// drains snapshots from the worker inside `sync`.
struct LiveStore {
    inner: StoreApp,
    worker: StoreWorker,
}

impl LiveStore {
    fn new(worker: StoreWorker) -> Self {
        Self {
            inner: StoreApp::new(),
            worker,
        }
    }
}

impl ui::widget::Widget for LiveStore {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        // Drain any snapshots the worker produced since last frame.
        while let Some(snap) = self.worker.try_snapshot() {
            if let Some(invocation) = snap.invocation {
                self.inner
                    .complete_lifecycle_invocation(invocation, snap.operation_error.clone());
            }
            let applies_items = match snap.query.as_deref() {
                Some(query) => query == self.inner.query(),
                None => snap.detail.is_none(),
            };
            if applies_items {
                self.inner.set_items(snap.items);
            }
            if let Some(detail) = snap.detail {
                self.inner.set_detail(detail);
            }
            self.inner.set_remotes(snap.remotes);
            self.inner.set_status_line(snap.status_line);
        }
        // `inner.sync` drains click edges into the pending-action
        // queue; hand each one to the worker with the full item so it
        // has the download URL etc.
        self.inner.sync(state, path);
        for (id, action) in self.inner.take_pending_actions() {
            if let Some(item) = self.inner.items().iter().find(|i| i.id == id).cloned() {
                self.worker.request(Request::Act(Box::new(item), action));
            }
        }
        for (id, action, invocation) in self.inner.take_lifecycle_actions() {
            if let Some(item) = self.inner.items().iter().find(|i| i.id == id).cloned() {
                if !self
                    .worker
                    .request_with_invocation(Request::Act(Box::new(item), action), Some(invocation))
                {
                    self.inner.complete_lifecycle_invocation(
                        invocation,
                        Some("store worker is unavailable".into()),
                    );
                }
            } else {
                self.inner.complete_lifecycle_invocation(
                    invocation,
                    Some("app is no longer present in the catalog".into()),
                );
            }
        }
        if self.inner.take_refresh_requested() {
            self.worker.request(Request::Refresh);
        }
        if let Some(invocation) = self.inner.take_lifecycle_refresh()
            && !self
                .worker
                .request_with_invocation(Request::Refresh, Some(invocation))
        {
            self.inner.complete_lifecycle_invocation(
                invocation,
                Some("store worker is unavailable".into()),
            );
        }
        if let Some(query) = self.inner.take_search_requested() {
            self.worker.request(Request::Search(query));
        }
        if let Some(id) = self.inner.take_detail_requested() {
            self.worker.request(Request::Detail(id));
        }
        for intent in self.inner.take_remote_intents() {
            let request = match intent {
                RemoteIntent::Refresh => Request::RefreshRemotes,
                RemoteIntent::Add { name, url } => Request::RemoteAdd { name, url },
                RemoteIntent::Remove(name) => Request::RemoteRemove(name),
            };
            self.worker.request(request);
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }
    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.inner.apply_layout(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
    }
    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.inner.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }
    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.inner
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.inner.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
    }
    fn children(&self) -> Vec<&dyn ui::widget::Widget> {
        self.inner.children()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn ui::widget::Widget + '_)> {
        self.inner.children_mut()
    }
    fn name(&self) -> &'static str {
        "LiveStore"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.inner.get_rect()
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }
    fn needs_animation_frame(&self) -> bool {
        self.worker.has_pending() || self.inner.needs_animation_frame()
    }
}
