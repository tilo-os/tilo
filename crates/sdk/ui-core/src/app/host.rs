// Frame layout and drawing shared by every host of App, and App's host-facing
// behaviour. The native event-loop runner is `app/native_runner.rs`.

/// Snapshot widget damage while its draw-updated geometry still describes the
/// scene being submitted. Callers deliberately clear dirty flags afterwards.
#[cfg(feature = "native")]
fn collect_submitted_widget_damage(widget: &dyn Widget, regions: &mut Vec<KurboRect>) {
    widget.collect_damage(regions);
}

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// Draws the root widget, its overlay, the action palette, and any visible
/// tooltip into `scene`. Returns the transform and logical window rect the
/// rest of the frame (legacy cursors, remote presence, debug outlines, HUD)
/// draws with, plus the draw-pass duration for the perf log line.
#[allow(
    clippy::too_many_lines,
    reason = "frame drawing is one ordered composition pass whose sequencing must remain explicit"
)]
#[cfg(feature = "native")]
fn draw_frame_content(
    app: &mut App,
    scene: &mut Scene,
    width: u32,
    height: u32,
    scale_factor: f32,
    t_draw_start: Option<crate::time::Instant>,
) -> (Affine, KurboRect, u128) {
    let __prof_resources = app.profiler.as_ref().map(|_| crate::time::now());
    app.refresh_control_action_availability();
    if let Some(p) = app.profiler.as_mut() {
        p.record_since(
            crate::profiling::Pass::ResourceAvailability,
            __prof_resources,
        );
    }
    // Widget rects are logical; this transform rasterises at the physical
    // resolution, so the frame stays crisp at any fractional scale.
    let transform = crate::scale::composite_transform(f64::from(scale_factor));
    let (vp_w, vp_h) = crate::scale::logical_size((width, height), f64::from(scale_factor));
    let window_rect = KurboRect::new(0.0, 0.0, vp_w, vp_h);
    let prev_vp = app.presence_ctx.push_viewport(window_rect);
    // No GPU damage clip: vello allocates a fresh `target_texture` per
    // frame, so clipping out a static region would just paint background
    // instead of preserving the last frame.
    let __prof_draw = app.profiler.as_ref().map(|_| crate::time::now());
    if let Some(history) = &mut app.history {
        history.draw(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    if app
        .history
        .as_ref()
        .is_none_or(|history| history.live_visible())
        && app.history_layout.is_none_or(|layout| layout.app_visible)
    {
        crate::widget::native_view_frame_start(app.root_widget.as_mut());
        app.root_widget.draw_with_presence(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
            &app.permission_ctx,
        );
    }
    if app.history_chrome_enabled {
        let context = crate::standalone_history::HistoryReadContext {
            document: &app.state.doc,
            active_record: app
                .persistence
                .records
                .as_ref()
                .and_then(|records| records.active()),
            document_identity: app.state.document_identity(),
            permissions: &app.permission_ctx,
        };
        if let Err(error) = app.document_history.draw(
            &context,
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        ) {
            tracing::debug!(?error, "history frame refused");
            app.layout.structure_dirty = true;
            app.layout.layout_dirty = true;
        }
    }
    // The window's top layer, after the app and the window's own chrome:
    // open menus, popovers and modals composite above everything drawn so
    // far, so a modal's scrim covers the History bar and every region. The
    // window stays the presence viewport: a modal covers all of it.
    if app
        .history
        .as_ref()
        .is_none_or(|history| history.live_visible())
        && app.history_layout.is_none_or(|layout| layout.app_visible)
    {
        app.root_widget.draw_overlay(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    if let Some(history) = &mut app.history {
        history.draw_overlay(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    if app.history_chrome_enabled {
        app.document_history.draw_overlay(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    if let Some(dialog) = app.close_admission.dialog.as_mut() {
        dialog.draw_overlay(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    app.presence_ctx.pop_viewport(prev_vp);
    if let Some(ap) = app
        .actions
        .action_palette
        .as_mut()
        .filter(|p| p.is_presented())
    {
        let win_w = f64::from(width) / f64::from(scale_factor);
        let card_w = (win_w * 0.6).clamp(360.0, 640.0);
        let x0 = ((win_w - card_w) / 2.0).max(0.0);
        let y0 = (f64::from(height) / f64::from(scale_factor) * 0.12).max(24.0);
        match app.palette_source {
            PaletteSource::Owned { .. } => ap.layout(&app.theme, Point::new(x0, y0), card_w),
            PaletteSource::Host => ap.layout_with_text(
                &app.theme,
                Point::new(x0, y0),
                card_w,
                &mut app.text_renderer,
            ),
        }
        ap.draw(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    draw_visible_tooltip(app, scene, transform, width, height, scale_factor);

    if let Some(p) = app.profiler.as_mut() {
        p.record_since(crate::profiling::Pass::Draw, __prof_draw);
    }
    let t_draw_us = t_draw_start.map_or(0, |t| t.elapsed().as_micros());
    (transform, window_rect, t_draw_us)
}

#[cfg(feature = "native")]
fn draw_visible_tooltip(
    app: &mut App,
    scene: &mut Scene,
    transform: Affine,
    width: u32,
    height: u32,
    scale_factor: f32,
) {
    app.input.tooltip_host.tick(crate::time::now());
    let Some((text, anchor)) = app
        .input
        .tooltip_host
        .visible()
        .map(|(label, anchor)| (crate::i18n::I18nManager::lookup(label, None), anchor))
    else {
        return;
    };
    let viewport = KurboRect::new(
        0.0,
        0.0,
        f64::from(width) / f64::from(scale_factor),
        f64::from(height) / f64::from(scale_factor),
    );
    crate::widget::tooltip::draw_hover_tooltip(
        scene,
        transform,
        &mut app.text_renderer,
        &app.theme,
        anchor,
        &text,
        viewport,
    );
}

/// Complete a native CPU frame only after drawing and damage collection.
/// Deferred or aborted frames retain return custody for the next completed
/// frame, so dirty state and accessibility publication stay tied to pixels.
#[cfg(feature = "native")]
fn complete_native_frame_content(app: &mut App, publish_accessibility: impl FnOnce(&mut App)) {
    app.root_widget.mark_drawn();
    publish_accessibility(app);
    app.finish_history_return_frame();
}

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// `None` means taffy layout computation failed and the caller must abandon
/// the frame; otherwise `(relayout_branch, compute_us, apply_us)` for the
/// perf log line.
#[allow(
    clippy::too_many_lines,
    reason = "one bounded solve/recompose/focus/mount lifecycle keeps ownership and final-frame ordering explicit"
)]
#[cfg(feature = "native")]
fn run_layout_pass(
    app: &mut App,
    root_node: NodeId,
    width: u32,
    height: u32,
    scale_factor: f32,
    structure_rebuilt: bool,
    native_host: Option<&crate::widget::NativeViewHost<'_>>,
) -> Option<(bool, u128, u128)> {
    use crate::widget::layout::{
        CompositionFocus, composition_geometry, settle_widget_composition,
    };
    let (content_origin, logical) = app.layout.safe_area.content(crate::scale::logical_size(
        (width, height),
        f64::from(scale_factor),
    ));
    app.layout.content_origin = content_origin;
    if app.close_admission.dialog.is_some() {
        app.layout_close_dialog(logical);
        return Some((true, 0, 0));
    }
    app.refresh_framework_history(logical.0);
    let viewport_changed = app.layout.last_layout_logical != Some(logical);
    if app.history_chrome_enabled {
        if (app.history_viewport_width >= 900.0) != (logical.0 >= 900.0) {
            app.layout.structure_dirty = true;
        }
        app.history_viewport_width = logical.0;
        let _ = app.history_local_resources();
        app.document_history
            .sync(&mut app.state.doc, &app.state.root);
    }
    if !app.layout.layout_dirty && !viewport_changed {
        return Some((false, 0, 0));
    }
    let mut t_compute_us = 0u128;
    let mut t_apply_us = 0u128;
    let mut current_root = root_node;
    let mut recomposed = false;
    let view = crate::view::capability::view_state(app.root_widget.as_ref());
    // A pending structural rebuild means the old map may already contain
    // removed controls. Capture only from an admitted, unchanged topology.
    app.layout.structure_dirty |= app.take_composition_dirty();
    let focus = if app.layout.structure_dirty {
        CompositionFocus::capture_live(app.root_widget.as_ref())
    } else {
        // SAFETY: the admitted maps belong to App's unchanged exclusive tree.
        unsafe { CompositionFocus::capture(app.focused_node, &app.layout.node_map, &app.bindings) }
    };
    let must_compute = structure_rebuilt
        || app.layout.structure_dirty
        || viewport_changed
        || scroll_opt_disabled()
        || app.root_widget.layout_needs_measure();
    let result = settle_widget_composition(viewport_changed, |rebuild| {
        if rebuild || app.layout.structure_dirty {
            recomposed = true;
            // Capture before build replaces controls. Shared view hooks carry
            // root-owned scroll/mode; persisted Scrollable offsets also survive.
            let autofocus = app.autofocus_on_structure_change;
            app.autofocus_on_structure_change = false;
            app.rebuild_structure();
            app.autofocus_on_structure_change = autofocus;
            if let Some(view) = &view {
                crate::view::capability::restore_view_state(app.root_widget.as_mut(), view);
            }
            // Sync may legitimately replace newly mounted delegates. Never
            // inspect or measure stale raw pointers from that build.
            if app.layout.structure_dirty {
                return Ok((None, true));
            }
            if let Some(focus) = &focus {
                // SAFETY: rebuild completed, and sync retained this topology.
                // A trap narrows eligible targets; it must not bypass identity
                // restoration or send saved selection to a fallback control.
                app.focused_node = unsafe {
                    focus.restore(
                        &app.layout.node_map,
                        &app.bindings,
                        Some(&app.focusable_order),
                    )
                };
                if app.accessibility.focus_trap_active && app.focused_node.is_none() {
                    app.set_focused_node(app.focusable_order.first().copied());
                }
            } else {
                // Anonymous retained widgets keep their own state. Resolve a
                // current admitted focus path; the old NodeId is invalid here.
                app.focused_node = unsafe {
                    CompositionFocus::current(
                        &app.layout.node_map,
                        &app.bindings,
                        Some(&app.focusable_order),
                    )
                };
            }
            current_root = app
                .layout
                .root_node
                .ok_or(crate::widget::WidgetLayoutError::CompositionDidNotSettle)?;
        }
        if must_compute || rebuild {
            let start = crate::time::now();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "Taffy uses f32 logical pixels"
            )]
            // SAFETY: App exclusively owns this exact stable tree and maps.
            unsafe {
                crate::widget::compute_widget_layout(
                    &mut app.layout.taffy,
                    current_root,
                    taffy::Size {
                        width: AvailableSpace::Definite(logical.0 as f32),
                        height: AvailableSpace::Definite(logical.1 as f32),
                    },
                    &app.layout.node_map,
                    &app.bindings,
                    &mut app.text_renderer,
                )?;
            }
            t_compute_us = t_compute_us.saturating_add(start.elapsed().as_micros());
        }
        let start = crate::time::now();
        app.apply_history_presentation(current_root);
        t_apply_us = t_apply_us.saturating_add(start.elapsed().as_micros());
        let dirty = app.take_composition_dirty();
        // Translation-only frames avoid both composition and a tree snapshot.
        let geometry = if viewport_changed {
            composition_geometry(&app.layout.taffy, current_root)?
        } else {
            vec![]
        };
        Ok((Some(geometry), dirty))
    });
    if let Err(error) = result {
        tracing::error!(?error, "widget layout did not settle; skipping frame");
        app.layout.layout_dirty = true;
        app.layout.last_layout_logical = None;
        return None;
    }
    if app.settle_history_presentation() {
        let root = app.layout.root_node?;
        return run_layout_pass(app, root, width, height, scale_factor, true, native_host);
    }
    // NativeView measurement is independent of platform attachment. Mount only
    // the successfully settled tree; mount publishes its retained geometry.
    if (structure_rebuilt || recomposed)
        && app
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
        && let Some(host) = native_host
    {
        crate::widget::mount_native_views(app.root_widget.as_mut(), host);
    }
    if app
        .history
        .as_ref()
        .is_some_and(|history| !history.live_interactive())
    {
        crate::widget::unmount_native_views(app.root_widget.as_mut());
    }
    app.layout.last_layout_logical = Some(logical);
    app.layout.layout_dirty = false;
    if must_compute || recomposed {
        app.update_focusable_order();
    }
    app.restore_history_focus();
    if app
        .focused_node
        .is_some_and(|node| !app.focusable_order.contains(&node))
    {
        app.set_focused_node(app.focusable_order.first().copied());
    }
    if let Some(profiler) = app.profiler.as_mut() {
        for (pass, micros) in [
            (crate::profiling::Pass::BuildLayout, t_compute_us),
            (crate::profiling::Pass::ApplyLayout, t_apply_us),
        ] {
            profiler.record(
                pass,
                crate::time::Duration::from_micros(u64::try_from(micros).unwrap_or(u64::MAX)),
            );
        }
    }
    // Initial redraws and programmatic changes need the same semantic door
    // as input. Publish only after the retained control tree has settled;
    // the publisher diffs the surface and checks broker connectivity.
    app.publish_action_surface();
    Some((true, t_compute_us, t_apply_us))
}

#[cfg(test)]
#[path = "window_title_tests.rs"]
mod window_title_tests;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "tests assert exact assigned values without floating-point computation"
)]
#[path = "pointer_broadcast_tests.rs"]
mod pointer_broadcast_tests;

#[cfg(test)]
#[path = "rebuild_focus_tests.rs"]
mod rebuild_focus_tests;

/// Mutating events should route through this so denied interactions
/// short-circuit before the widget ever observes them.
pub fn dispatch_gated_event<W: crate::widget::Widget + ?Sized>(
    ctx: &crate::permissions::PermissionContext,
    widget: &W,
    dispatch: impl FnOnce(),
) -> bool {
    if let Some(scope) = crate::permissions::capability::permission_scope(widget)
        && !ctx.check(&scope)
    {
        return false;
    }
    dispatch();
    true
}

/// The focus-narration path (`set_focused_node`) is private, so these tests
/// drive it through the public `init_layout` (autofocus) + the a11y action
/// dispatch / setter API.
#[cfg(test)]
#[path = "screen_reader_tests.rs"]
mod screen_reader_tests;

/// The modal's open flag is a state change (no structural rebuild), so node
/// ids stay stable and `refresh_focus_order` re-evaluates the trap.
#[cfg(test)]
#[path = "focus_trap_tests.rs"]
mod focus_trap_tests;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact preference values are part of the test contract"
)]
#[path = "low_vision_tests.rs"]
mod low_vision_tests;

#[cfg(test)]
#[path = "fullscreen_tests.rs"]
mod fullscreen_tests;

#[cfg(test)]
#[path = "action_palette_tests.rs"]
mod action_palette_tests;

#[cfg(test)]
#[path = "search_open_tests.rs"]
mod search_open_tests;

// See `crate::view` for the (de)serialize-as-a-whole design this implements.
impl crate::view::FreezeThaw for App {
    /// `AutoCommit::save` takes `&mut self` to flush its open transaction;
    /// freeze is `&self` on a cold path, so save a clone rather than widen
    /// the signature.
    fn serialize_state(&self) -> Vec<u8> {
        self.state.doc.clone().save()
    }

    /// Also resets the undo history — the old stacks name changes this
    /// document has never seen.
    fn restore_state(&mut self, bytes: &[u8]) -> Result<(), crate::view::ThawError> {
        let doc = automerge::AutoCommit::load(bytes)
            .map_err(|e| crate::view::ThawError::State(e.to_string()))?;
        self.state.adopt_doc(doc);
        Ok(())
    }

    fn capture_view(&self) -> crate::view::View {
        let _entry = self.enter_wake_scope();
        let ordinal = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        let focus = crate::view::capture_focus(ordinal, || {
            self.focused_node.and_then(|id| {
                self.inspect_node(id, |widget| crate::undo::get_state(widget))
                    .ok()
                    .flatten()
            })
        });
        let viewport = self
            .layout
            .last_frame_size
            .map(|s| crate::view::ViewRect {
                x: 0.0,
                y: 0.0,
                width: f64::from(s.width),
                height: f64::from(s.height),
            })
            .unwrap_or_default();
        crate::view::capture_widget_view_bound(&*self.root_widget, focus, viewport)
    }

    /// `open_resource` is carried for presence/reboot to reopen the record;
    /// record-navigation itself is the host's and is not driven here.
    fn restore_view(&mut self, view: &crate::view::View) {
        let _entry = self.enter_wake_scope();
        crate::view::restore_widget_view_bound(&mut *self.root_widget, view);

        if let Some(f) = &view.focus
            && let Some(&id) = self.focusable_order.get(f.ordinal)
        {
            self.set_focused_node(Some(id));
            let _ = self.mutate_node(id, |widget| {
                crate::view::restore_focus_state_bound(widget, f);
            });
        }

        self.layout.layout_dirty = true;
        self.needs_sync = true;
    }
}

// View presence: exercised on the real broadcast/receive path —
// `broadcast_view_presence` through the presence sink, and
// `apply_collab_event_for_test` through `apply_remote_presence`.
#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "serialized presence fields must round-trip exactly"
)]
#[path = "view_presence_tests.rs"]
mod view_presence_tests;

#[cfg(test)]
#[path = "semantic_tree_app_tests.rs"]
mod semantic_tree_app_tests;

#[cfg(test)]
#[path = "damage_lifecycle_tests.rs"]
mod damage_lifecycle_tests;

#[cfg(test)]
#[path = "wake_integration_tests.rs"]
mod wake_integration_tests;

#[cfg(test)]
#[path = "bound_component_tests.rs"]
mod bound_component_tests;

#[cfg(test)]
pub(crate) fn draw_prepared_component_test_frame(app: &mut App) {
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    app.layout.layout_dirty = true;
    run_layout_pass(app, root, 800, 600, 1.0, true, None).unwrap();
    let mut scene = Scene::new();
    draw_frame_content(app, &mut scene, 800, 600, 1.0, None);
}

#[cfg(test)]
#[path = "prepared_runner_tests.rs"]
mod prepared_runner_tests;

#[cfg(test)]
#[path = "card_grid_host_tests.rs"]
mod card_grid_host_tests;

#[cfg(test)]
#[path = "viewport_composition_tests.rs"]
mod viewport_composition_tests;

#[cfg(test)]
#[path = "card_composition_tests.rs"]
mod card_composition_tests;

#[cfg(test)]
#[path = "form_composition_tests.rs"]
mod form_composition_tests;

#[cfg(test)]
#[path = "notes_rich_controls_tests.rs"]
mod notes_rich_controls_tests;
