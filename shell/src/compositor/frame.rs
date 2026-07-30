//! Per-frame scene builder shared between `bin/shots` and the native
//! compositor.
//!
//! Both consumers need exactly the same set of operations:
//!
//!   1. Build a `ShellOverlayHost` plus its shared `Chrome` state from a
//!      `WorkspaceTree`.
//!   2. Maintain a `TaffyTree` keyed by widget pointers.
//!   3. Run the layout pass (`compute_layout_with_measure` →
//!      `apply_layout`) at a given viewport size.
//!   4. Run the draw pass into a fresh `vello::Scene`.
//!
//! `bin/shots`'s [`ShellDriver`] (in `bin/shots/driver.rs`) wraps a
//! [`FrameBuilder`] and adds the bits scenarios need on top: a winit
//! `Modifiers` snapshot, a focus cursor, the cached last-cursor point,
//! and the click/drag/key synthesis helpers.
//!
//! The native compositor (`compositor::native::run`) only ever needs
//! the layout-and-paint core, so it talks to a [`FrameBuilder`]
//! directly. Lifting this module out of `bin/shots` is the contract the
//! Phase 4 plan ("Adapt the shots driver for compositor frames") asks
//! for: both code paths render through the same builder, so the
//! pixel output is byte-identical.
//!
//! See `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`
//! Phase 4, Task 4.1.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{Modifiers, WindowEvent};

use crate::shell::build_nirios_overlay_host_with_chrome;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::WorkspaceTree;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::shell::overlay_host::OutputGeometry;

/// Layout + paint core shared between `bin/shots`'s `ShellDriver` and
/// the native compositor's per-frame tick.
///
/// Owning this struct gives the caller the ability to mutate chrome
/// state (overlays, palette query, ws-settings tab) and re-paint —
/// nothing more. Input dispatch, scenario stepping and scene catalogues
/// stay in `bin/shots/driver.rs` because the compositor doesn't need
/// them: the compositor receives input through the smithay seat layer
/// and translates it into mutations on the same `Chrome`.
pub struct FrameBuilder {
    host: ShellOverlayHost,
    chrome: Chrome,
    taffy: TaffyTree<()>,
    /// Widget-pointer map captured during `build_layout` — kept alive
    /// so the measure closure inside `compute_layout_with_measure` can
    /// re-borrow each frame.
    node_map: HashMap<NodeId, *mut dyn Widget>,
    /// Inner node — the host's own root child. Used as the target of
    /// `apply_layout` so the host's rect tree is rooted at the
    /// viewport origin.
    host_node: Option<NodeId>,
    /// Outer node — a viewport-sized wrapper that gives `compute_layout`
    /// an explicit root size. Taffy needs a definite extent; the host
    /// itself is `percent(1.0)` so we wrap it.
    root_node: Option<NodeId>,
    text_renderer: TextRenderer,
    theme: Theme,
    viewport: (f64, f64),
    /// The displays behind `viewport`, when the caller knows them. The
    /// viewport is the *bounding box* of every output, so on a multi-monitor
    /// desktop it is not a display anyone actually has — a window group
    /// sized as a fraction of it would span both monitors. `None` means "one
    /// display, the size of the viewport", which is true for winit, headless
    /// and every single-output DRM setup.
    outputs: Option<Vec<OutputGeometry>>,
}

impl FrameBuilder {
    /// Build a frame builder around the canonical bootstrap workspace
    /// tree at `viewport` size.
    pub fn new(viewport: (u32, u32)) -> Self {
        Self::with_tree(WorkspaceTree::bootstrap_default(), viewport)
    }

    /// Build a frame builder around an explicit `WorkspaceTree` —
    /// used by `bin/shots`'s `window` pre-roll path (which loads tree
    /// state from disk first).
    pub fn with_tree(tree: WorkspaceTree, viewport: (u32, u32)) -> Self {
        Self::from_parts(build_nirios_overlay_host_with_chrome(&tree), viewport)
    }

    /// Optional theme override for offscreen `shots` capture:
    /// `QOS_SHOTS_THEME=light|high-contrast|dark`. Unset (production) keeps the
    /// dark default, so this is a no-op off the shots harness. Lets a
    /// theme-driven chrome change be proven in light + high-contrast, not just
    /// dark — the renderers under `apps/*` grew the same hook.
    fn shots_theme_override() -> Option<Theme> {
        let raw = std::env::var("QOS_SHOTS_THEME").ok()?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Theme::light()),
            "high-contrast" | "high_contrast" | "hc" => Some(Theme::high_contrast()),
            "dark" => Some(Theme::dark()),
            _ => None,
        }
    }

    /// Build a frame builder over a *shared* [`WorkspaceTreeHandle`],
    /// with the peer-sync palette rows on or off.
    ///
    /// The compositor needs this rather than [`Self::with_tree`]
    /// because peer-sync writes into the same tree the chrome reads:
    /// a replica applying a peer's change and the strip that paints it
    /// have to be looking at one `Arc<RwLock<_>>`, not two copies.
    pub fn with_handle(
        tree: crate::shell::state::WorkspaceTreeHandle,
        enable_peer_sync: bool,
        viewport: (u32, u32),
    ) -> Self {
        Self::from_parts(
            crate::shell::build_nirios_overlay_host_with_handle(tree, enable_peer_sync),
            viewport,
        )
    }

    fn from_parts(
        (host, chrome): (ShellOverlayHost, Chrome),
        viewport: (u32, u32),
    ) -> Self {
        let theme = Self::shots_theme_override().unwrap_or_else(Theme::dark);
        let mut builder = Self {
            host,
            chrome,
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            host_node: None,
            root_node: None,
            text_renderer: TextRenderer::new(),
            theme,
            viewport: (viewport.0 as f64, viewport.1 as f64),
            outputs: None,
        };
        builder.rebuild_layout_tree();
        builder.relayout();
        builder
    }

    /// The displays to resolve window-group widths against: whatever the
    /// compositor last reported, else a single display the size of the
    /// viewport.
    fn effective_outputs(&self) -> Vec<OutputGeometry> {
        self.outputs.clone().unwrap_or_else(|| {
            vec![OutputGeometry {
                x: 0.0,
                width: self.viewport.0,
                height: self.viewport.1,
            }]
        })
    }

    /// Tell the frame builder which displays it is painting across, so a
    /// group's width is a fraction of its own monitor rather than of the
    /// combined desktop. Called by the compositor at startup and on every
    /// output hotplug / mode change.
    pub fn set_output_geometry(&mut self, outputs: Vec<OutputGeometry>) {
        if self.outputs.as_deref() != Some(outputs.as_slice()) {
            self.outputs = Some(outputs);
            self.rebuild_layout_tree();
        }
    }

    fn rebuild_layout_tree(&mut self) {
        self.taffy = TaffyTree::new();
        self.node_map.clear();
        // Push display geometry before the tree is built: the strips read it
        // to resolve each group's width fraction in this same pass.
        let outputs = self.effective_outputs();
        self.host.set_output_geometry(&outputs);
        let child = self
            .host
            .build_layout(&mut self.taffy, &self.theme, &mut self.node_map);
        let (w, h) = self.viewport;
        let root = self
            .taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w as f32),
                        height: Dimension::length(h as f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .expect("frame-builder root node");
        self.host_node = Some(child);
        self.root_node = Some(root);
    }

    /// Run compute_layout + apply_layout against the current tree. The
    /// host's animation state is preserved because we don't rebuild
    /// the taffy tree — only the layout pass re-runs.
    ///
    /// If the host reports `take_structure_dirty()` the taffy tree is
    /// rebuilt first so the new structure participates in layout. This
    /// mirrors `App::handle_window_event`'s post-event sweep.
    pub fn relayout(&mut self) {
        // Apply the compositor's live fullscreen state to the strip rows first.
        // This flags the owning row structurally dirty on a real change, so the
        // `take_structure_dirty` check below rebuilds the taffy tree and the
        // full-output collapse (or its restore) takes effect this frame.
        self.host.sync_fullscreen_from_chrome();
        if self.host.take_structure_dirty() {
            self.rebuild_layout_tree();
        }
        let (w, h) = self.viewport;
        let outer = self.root_node.expect("rebuild_layout_tree first");
        let child = self.host_node.expect("rebuild_layout_tree first");
        // D-2 fix: previously this closure captured `let measure_map =
        // self.node_map.clone();`, which copied the entire widget-ptr
        // map on every relayout (50-100 entries on a typical frame —
        // visible cost on a continuous resize). The closure only needs
        // read access, so we borrow `&self.node_map` directly. The
        // borrow is disjoint from `self.taffy` and `self.text_renderer`
        // (separate fields) so the borrow checker is happy.
        let measure_map: &HashMap<NodeId, *mut dyn Widget> = &self.node_map;
        let tr = &mut self.text_renderer;
        self.taffy
            .compute_layout_with_measure(
                outer,
                Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("frame-builder compute_layout");
        self.host.apply_layout(&self.taffy, child, Point::ORIGIN);
    }

    /// Run the widget tree's per-frame state pump: `sync` (pull external state
    /// into the CRDT) then `update` (advance widget-local state). Returns
    /// `true` when something changed and the frame should be repainted.
    ///
    /// `ui::App::run` does this every frame for standalone apps, but the
    /// compositor's frame loop only ever laid out and painted — so a hosted app
    /// that depends on the pump never advanced. The visible symptom was a live
    /// terminal that stayed on its empty placeholder forever: `sync` is what
    /// drains the PTY into the grid, so without it no shell output ever
    /// arrived. Apps whose content is captured at construction (the notes seed,
    /// the file listing) looked fine, which is why only the terminal appeared
    /// "not real".
    pub fn pump_apps(&mut self, doc: &mut AutoCommit, root: &ObjId) -> bool {
        self.host.sync(doc, root);
        self.host.update(doc, root)
    }

    /// Render the current frame into a fresh Vello scene. Caller is
    /// responsible for handing it to a renderer (offscreen / live /
    /// compositor backend).
    pub fn paint(&mut self) -> Scene {
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        self.host.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
            &presence,
        );
        scene
    }

    /// Convenience for the offscreen scene-set path: `relayout` then
    /// `paint`. Used when state mutations between scenes can change
    /// structural layout (overlay reservations).
    pub fn paint_settled(&mut self) -> Scene {
        self.relayout();
        self.paint()
    }

    /// Mutate chrome state — opens overlays, switches tabs, tweaks
    /// quick-settings, etc. Borrows the chrome internally so callers
    /// don't have to juggle the `RefCell`.
    pub fn with_chrome<F: FnOnce(&mut ChromeState)>(&mut self, f: F) {
        let mut st = self.chrome.borrow_mut();
        f(&mut st);
    }

    /// Read-only handle to the chrome cell.
    pub fn chrome(&self) -> std::cell::Ref<'_, ChromeState> {
        self.chrome.borrow()
    }

    /// Promote a window group to full-output (a surface that requested
    /// `xdg_toplevel.set_fullscreen`), or clear the fullscreen state with
    /// `None`. The compositor calls this from its `XdgShellHandler`
    /// fullscreen/unfullscreen path each time the request set changes; the next
    /// `relayout` collapses the layout to that group (top bar + sidebar + other
    /// strips suppressed) and a later `None` restores the exact prior tiling.
    pub fn set_fullscreen(&mut self, group: Option<u64>) {
        self.with_chrome(|st| st.set_fullscreen_group(group));
    }

    /// The group currently promoted to full-output, or `None` in the normal
    /// tiled state. Exposed for the compositor + tests.
    pub fn fullscreen_group(&self) -> Option<u64> {
        self.chrome().fullscreen_group()
    }

    /// Cloneable handle to the shared chrome cell.
    pub fn chrome_handle(&self) -> Chrome {
        self.chrome.clone()
    }

    pub fn host(&self) -> &ShellOverlayHost {
        &self.host
    }

    /// Whether the widget tree has an animation in flight (overlay
    /// transitions, spring settles, spawn/close fades, the wheel disc,
    /// …). The native render loop uses this to decide whether the next
    /// frame needs repainting, so it can idle instead of burning a core
    /// repainting an unchanged scene.
    pub fn needs_animation_frame(&self) -> bool {
        self.host.needs_animation_frame()
    }

    /// The chrome's monotonic revision counter. Bumps whenever chrome
    /// state mutates (overlay toggles, notifications, tab switches, …).
    /// The render loop repaints when this changes.
    pub fn chrome_revision(&self) -> u64 {
        self.chrome.borrow().revision
    }

    pub fn host_mut(&mut self) -> &mut ShellOverlayHost {
        &mut self.host
    }

    /// On-screen rectangle of the tile currently painting the foreign
    /// wayland surface `surface_id`, or `None` when no visible tile hosts
    /// it. See [`ShellOverlayHost::foreign_surface_rect`].
    pub fn foreign_surface_rect(&mut self, surface_id: u64) -> Option<vello::kurbo::Rect> {
        self.host.foreign_surface_rect(surface_id)
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn viewport(&self) -> (f64, f64) {
        self.viewport
    }

    /// Inner node id (the host's root child). Required by the input
    /// dispatch path inside `bin/shots`'s `ShellDriver` — the compositor
    /// path doesn't currently need this.
    pub fn host_node(&self) -> NodeId {
        self.host_node.expect("rebuild_layout_tree first")
    }

    /// Read-only access to the taffy tree — also for input dispatch.
    pub fn taffy(&self) -> &TaffyTree<()> {
        &self.taffy
    }

    /// Mutable access to the text renderer. Required by the input
    /// dispatch / scenario step paths inside `bin/shots`'s
    /// `ShellDriver` — the compositor path doesn't currently need
    /// this either.
    pub fn text_renderer_mut(&mut self) -> &mut TextRenderer {
        &mut self.text_renderer
    }

    /// Resize the viewport. Triggers a structural rebuild on the next
    /// `relayout` because the wrapper root node carries an explicit
    /// extent. Used by the compositor's `BackendEvent::OutputResized`
    /// handler.
    pub fn set_viewport(&mut self, viewport: (u32, u32)) {
        self.viewport = (viewport.0 as f64, viewport.1 as f64);
        self.rebuild_layout_tree();
    }

    /// Split the host out from the taffy tree as disjoint borrows.
    /// `ShellOverlayHost::handle_event` needs `&mut self` *plus* a
    /// `&TaffyTree`, both of which live inside `FrameBuilder` —
    /// callers that go through accessor methods can't satisfy the
    /// borrow checker without this helper.
    pub fn host_and_taffy_mut(&mut self) -> (&mut ShellOverlayHost, &TaffyTree<()>) {
        (&mut self.host, &self.taffy)
    }

    /// Dispatch a single `WindowEvent` through the host's `handle_event`
    /// pipeline. Returns whether the event was consumed.
    ///
    /// The compositor's `Modifiers`, `AutoCommit` doc, `ObjId`, and
    /// focus cursor live outside the FrameBuilder (each consumer
    /// owns its own — `ShellDriver` for in-process scenarios,
    /// `native::run` for the live compositor) — they're passed in
    /// rather than stored here so the FrameBuilder stays a pure
    /// layout/paint core shared by both.
    ///
    /// Both `ShellDriver::dispatch` and the compositor's
    /// `pump_test_control_conn` route through this one method so
    /// the wire to the host is shared in one place.
    pub fn dispatch_event(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root_obj: &ObjId,
        focus: &mut Option<NodeId>,
    ) -> bool {
        let mut handled = false;
        let child = self.host_node();
        // Snapshot the focus going in so we can detect any new target
        // set by the host's `handle_event`. Mirrors what
        // `ui::App::window_event` does at the production seam.
        let prior_focus = *focus;
        let mut focus_target: Option<NodeId> = None;
        {
            let (host, taffy) = self.host_and_taffy_mut();
            host.handle_event(
                event,
                1.0,
                modifiers,
                state,
                root_obj,
                &mut handled,
                &mut focus_target,
                child,
                taffy,
            );
        }
        // Apply the requested focus change: drop old widget's focus,
        // raise new widget's focus. Without this step, clicking on a
        // focusable widget like NotesApp would set `focus_target` but
        // the widget itself would never see `set_focus(true)` — its
        // `is_focused()` stays `false` and rendering keeps showing the
        // unfocused state ("Click to edit" instead of editing chrome).
        if let Some(new_id) = focus_target {
            if prior_focus != Some(new_id) {
                if let Some(old_id) = prior_focus {
                    if let Some(&w_ptr) = self.node_map.get(&old_id) {
                        unsafe {
                            (*w_ptr).set_focus(false);
                        }
                    }
                }
                if let Some(&w_ptr) = self.node_map.get(&new_id) {
                    unsafe {
                        (*w_ptr).set_focus(true);
                    }
                }
                *focus = Some(new_id);
            }
        }
        handled
    }

    /// Snapshot of every addressable widget in the live tree, keyed by
    /// either its opt-in `test_id` or a stable path
    /// (`Parent/Child[index]`). Same shape `ShellDriver::widget_registry`
    /// uses for in-process scenarios; lifted into `FrameBuilder` so the
    /// out-of-process control protocol (`compositor::test_control`) can
    /// reach the same data without going through the shots binary.
    ///
    /// Walked fresh on every call — there's no hidden cache. Callers
    /// that need the registry across multiple actions should re-fetch
    /// after each frame's `paint`/`relayout` (layout can shift rects
    /// between frames).
    pub fn widget_registry(&self) -> HashMap<String, vello::kurbo::Rect> {
        let mut out = HashMap::new();
        let mut sibling_counts: HashMap<String, usize> = HashMap::new();
        walk_widget(&self.host, "", &mut sibling_counts, &mut out);
        self.register_chrome_rects(&mut out);
        out
    }

    /// Register the chrome affordances that are painted by the overlay
    /// host rather than by a child widget, so they can be clicked by
    /// name.
    ///
    /// These are drawn straight into the scene and hit-tested from
    /// cached rects, so a tree walk cannot see them: the overview's
    /// add-workspace tile, its workspace cards, the workspace-settings
    /// tab pills and name field, the Permissions tab's Revoke buttons,
    /// the connect prompt's buttons. `ShellDriver` had a hand-curated
    /// table covering three of them; a rect that only one of the two
    /// harnesses can address is a rect only half the tests can reach,
    /// so the list lives here and both read it.
    ///
    /// Zero-area rects are skipped, which is what makes a name mean
    /// "on screen now" — the overview's tile is unaddressable while the
    /// overview is closed rather than clickable at the origin.
    fn register_chrome_rects(&self, out: &mut HashMap<String, vello::kurbo::Rect>) {
        use crate::shell::chrome_state::WsTab;
        let host = &self.host;
        let mut put = |name: String, rect: vello::kurbo::Rect| {
            if rect.area() > 0.0 {
                out.insert(name, rect);
            }
        };
        put("add_workspace".into(), host.add_workspace_rect_for_tests());
        for (key, tab) in [
            ("general", WsTab::General),
            ("members", WsTab::Members),
            ("permissions", WsTab::Permissions),
        ] {
            put(
                format!("ws_settings_tab[{key}]"),
                host.ws_settings_tab_pill_rect(tab),
            );
        }
        put(
            "ws_settings_name_input".into(),
            host.ws_settings_name_rect_for_tests(),
        );
        for (idx, rect) in host.overview_row_rects_for_tests().into_iter().enumerate() {
            put(format!("overview_card[{idx}]"), rect);
        }
        // Revoke buttons are addressable both by position (stable for a
        // harness that just wants "the first one") and by the capability
        // id they revoke (stable across reordering).
        for (index, (cap_id, rect)) in host.permissions_revoke_rects().iter().enumerate() {
            put(format!("permissions_revoke[{index}]"), *rect);
            put(format!("permissions_revoke[{cap_id}]"), *rect);
        }
        if let Some(composer) = host.grant_composer_rects() {
            put("grant_subject".into(), composer.subject);
            put("grant_capability".into(), composer.capability);
            put("grant_submit".into(), composer.submit);
            for (key, rect) in [
                ("always", composer.expiry[0]),
                ("session", composer.expiry[1]),
                ("once", composer.expiry[2]),
            ] {
                put(format!("grant_expiry[{key}]"), rect);
            }
        }
        for (index, (user, role_rect, remove_rect)) in
            host.member_control_rects().iter().enumerate()
        {
            put(format!("member_role[{index}]"), *role_rect);
            put(format!("member_role[{user}]"), *role_rect);
            put(format!("member_remove[{index}]"), *remove_rect);
            put(format!("member_remove[{user}]"), *remove_rect);
        }
        for (which, rect) in host.peer_sync_prompt_rects_for_tests() {
            put(format!("peer_sync_{which}"), rect);
        }
    }
}

/// Depth-first walk of the widget tree. Registers each visited node:
///   - if `test_id()` returns `Some(id)`, `id -> rect` (highest priority
///     — overrides any path-based key).
///   - the path-based key `Parent/.../Self[sibling_index]`, so
///     un-tagged widgets stay addressable.
///   - a bare-name form (`Parent/Self`) for the first sibling of a
///     given name.
///
/// `sibling_counts` is a parent-scoped counter map keyed by
/// `parent_path::name`. Children of the same parent that share a name
/// get deterministic `[0]`, `[1]`, ... suffixes. The shots `ShellDriver`
/// uses this exact walk too — exposed `pub` here so both consumers
/// share one implementation.
pub fn walk_widget(
    w: &dyn ui::widget::Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<String, vello::kurbo::Rect>,
) {
    let name = w.name().to_string();
    let sibling_key = format!("{parent_path}::{name}");
    let idx = sibling_counts.entry(sibling_key).or_insert(0);
    let my_index = *idx;
    *idx += 1;
    let path = if parent_path.is_empty() {
        format!("{name}[{my_index}]")
    } else {
        format!("{parent_path}/{name}[{my_index}]")
    };
    out.insert(path.clone(), w.get_rect());
    if my_index == 0 {
        let bare = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        };
        out.entry(bare).or_insert_with(|| w.get_rect());
    }
    if let Some(id) = w.test_id() {
        out.insert(id.to_string(), w.get_rect());
    }
    // Affordances the widget paints itself, addressable by name. Skipping
    // zero-area entries is what makes a name mean "on screen now".
    for (name, rect) in w.test_rects() {
        if rect.area() > 0.0 {
            out.insert(format!("{path}/{name}"), rect);
            out.entry(name).or_insert(rect);
        }
    }
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for child in w.children() {
        walk_widget(child, &path, &mut child_counts, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke: a fresh builder produces a non-empty Vello scene at
    /// 1600x900. The exact byte-count would change with any rendering
    /// tweak, so we only assert the scene encoded *something*.
    #[test]
    fn frame_builder_paints_non_empty_scene() {
        let mut fb = FrameBuilder::new((1600, 900));
        let scene = fb.paint();
        assert!(
            !scene.encoding().is_empty(),
            "fresh FrameBuilder should produce a non-empty Vello scene"
        );
    }

    /// `with_chrome` mutations are visible through `chrome()` — the
    /// shared cell is the same one the host borrows during draw.
    #[test]
    fn frame_builder_chrome_round_trips() {
        let mut fb = FrameBuilder::new((1600, 900));
        assert!(!fb.chrome().overlays.overview);
        fb.with_chrome(|st| st.overlays.overview = true);
        assert!(fb.chrome().overlays.overview);
    }

    /// `set_viewport` rebuilds the layout tree without panicking and
    /// keeps `paint` working.
    #[test]
    fn frame_builder_resize_paints_again() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint();
        fb.set_viewport((1280, 720));
        fb.relayout();
        let scene = fb.paint();
        assert!(!scene.encoding().is_empty());
    }

    /// Look up a widget's rect in a `widget_registry` snapshot by its *last*
    /// path segment (`.../TopBarChrome[0]` → `TopBarChrome`). The registry
    /// keys are full paths, so an exact-key lookup on a nested widget would
    /// miss; this matches the leaf name (with any `[idx]` suffix stripped).
    /// The widgets asserted on here (top bar, the single active strip row) are
    /// unique in the rendered tree, so the first match is deterministic.
    #[cfg(test)]
    fn rect_named(reg: &HashMap<String, vello::kurbo::Rect>, name: &str) -> vello::kurbo::Rect {
        reg.iter()
            .find(|(k, _)| {
                let seg = k.rsplit('/').next().unwrap_or(k);
                let leaf = match seg.find('[') {
                    Some(i) => &seg[..i],
                    None => seg,
                };
                leaf == name
            })
            .map(|(_, r)| *r)
            .unwrap_or_else(|| {
                panic!(
                    "widget `{name}` not found in registry; keys: {:?}",
                    reg.keys().collect::<Vec<_>>()
                )
            })
    }

    /// A group promoted to fullscreen absorbs the whole output — the top bar
    /// collapses and the strip's rect rises to the top of the output and spans
    /// its width — and clearing it restores the exact prior layout. This is the
    /// compositor-scene half of the "fullscreen enters / exits and restores the
    /// tiling" contract, driven through the real `FrameBuilder::set_fullscreen`
    /// → `ChromeState` → overlay-host path.
    #[test]
    fn set_fullscreen_promotes_group_and_restores_on_exit() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();

        // A group that actually exists in the rendered tree.
        let gid = {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            tree.active_workspace()
                .expect("bootstrap tree has an active workspace")
                .strips[0]
                .window_groups[0]
                .id
        };

        let before = fb.widget_registry();
        let topbar_before = rect_named(&before, "TopBarChrome");
        let row_before = rect_named(&before, "GroupRow");
        assert!(
            topbar_before.height() > 10.0,
            "top bar visible before fullscreen"
        );

        // Enter fullscreen on that group.
        fb.set_fullscreen(Some(gid));
        assert_eq!(fb.fullscreen_group(), Some(gid));
        let _ = fb.paint_settled();
        assert!(fb.chrome().is_fullscreen());

        let during = fb.widget_registry();
        let topbar_fs = rect_named(&during, "TopBarChrome");
        let row_fs = rect_named(&during, "GroupRow");
        assert!(
            topbar_fs.height() <= 0.5,
            "top bar collapses under fullscreen; got height {}",
            topbar_fs.height(),
        );
        assert!(
            row_fs.y0 < row_before.y0 - 20.0,
            "the strip rises to the top once the bar is gone; before y0 {} during {}",
            row_before.y0,
            row_fs.y0,
        );
        assert!(
            row_fs.width() >= fb.viewport().0 - 1.0,
            "fullscreen strip spans the output width; got {} vs {}",
            row_fs.width(),
            fb.viewport().0,
        );

        // Exit — the layout restores exactly.
        fb.set_fullscreen(None);
        assert_eq!(fb.fullscreen_group(), None);
        let _ = fb.paint_settled();
        assert!(!fb.chrome().is_fullscreen());

        let after = fb.widget_registry();
        let topbar_after = rect_named(&after, "TopBarChrome");
        let row_after = rect_named(&after, "GroupRow");
        assert!(
            (topbar_after.height() - topbar_before.height()).abs() < 0.5,
            "top bar height restores: before {} after {}",
            topbar_before.height(),
            topbar_after.height(),
        );
        assert!(
            (row_after.y0 - row_before.y0).abs() < 0.5
                && (row_after.x0 - row_before.x0).abs() < 0.5
                && (row_after.width() - row_before.width()).abs() < 0.5,
            "strip row rect restores exactly: before {row_before:?} after {row_after:?}",
        );
    }

    /// A fullscreen id that the rendered tree does NOT host must be a safe
    /// no-op — it must not blank the shell by hiding every strip. (The live
    /// client tree and the rendered tree are not yet unified.)
    #[test]
    fn set_fullscreen_with_unknown_group_does_not_blank_shell() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        let before = fb.widget_registry();
        let row_before = rect_named(&before, "GroupRow");

        // An id that cannot exist in the tree.
        fb.set_fullscreen(Some(u64::MAX));
        let _ = fb.paint_settled();

        let during = fb.widget_registry();
        let row_during = rect_named(&during, "GroupRow");
        let topbar_during = rect_named(&during, "TopBarChrome");
        assert!(
            row_during.width() > 0.0 && row_during.height() > 0.0,
            "an unknown fullscreen id must not collapse the shell; got {row_during:?}",
        );
        // Geometry matches the normal (non-fullscreen) layout — no promotion,
        // no chrome suppression.
        assert!((row_during.width() - row_before.width()).abs() < 0.5);
        assert!(
            topbar_during.height() > 10.0,
            "top bar stays visible for an unknown fullscreen id",
        );
    }

    /// D-2 perf-fix contract: `widget_registry` is idempotent — two
    /// consecutive calls (no layout / chrome mutation between them)
    /// return the same set of keys mapped to the same rects. This
    /// mirrors the contract before the relayout-clone perf fix: the
    /// walker may not mutate anything, so subsequent reads must see
    /// the same data.
    #[test]
    fn widget_registry_is_idempotent_between_calls() {
        let mut fb = FrameBuilder::new((1600, 900));
        // Settle the layout once so rects are populated.
        let _ = fb.paint();
        let snap_a = fb.widget_registry();
        let snap_b = fb.widget_registry();
        assert_eq!(
            snap_a.len(),
            snap_b.len(),
            "widget_registry should return the same number of entries \
             across repeated calls"
        );
        for (key, rect) in &snap_a {
            assert_eq!(
                snap_b.get(key),
                Some(rect),
                "widget_registry key `{key}` mismatched between calls: \
                 {:?} vs {:?}",
                rect,
                snap_b.get(key)
            );
        }
    }
}
