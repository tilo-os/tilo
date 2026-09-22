// Native window, render-thread, accessibility-adapter, and surface host.
//
// This is the platform host around App; application semantics remain on App
// and its responsibility-specific implementation units.

/// wgpu `RenderContext`, `RenderSurface`, and `vello::Renderer` live on a
/// dedicated worker thread (see [`crate::render_thread`]) — this struct
/// keeps only the channel ends to ship frames and recycle scene buffers.
struct AppHandler {
    app: App,
    /// `None` until the first `resumed` — every modern platform gates window
    /// creation behind an `ActiveEventLoop`.
    window: Option<Arc<Window>>,
    /// `None` again in `exiting`, after the worker is joined, so its surface
    /// drops BEFORE the `Window` arc goes away.
    render: Option<RenderThreadHandle>,
    /// A real, separate fullscreen output for a root widget's audience view.
    /// It is deliberately not modelled as a floating surface: it has no
    /// desktop-space proxy or compositor handoff and is always destroyed
    /// before its renderer/window during stop and app teardown.
    audience_presentation: Option<AudiencePresentationWindow>,
    floating_windows: Vec<NativeFloatingWindow>,
    /// `None` until `resumed` creates the window (must be built while
    /// hidden); lazy, so the hot path pays nothing with no screen reader
    /// running.
    a11y_adapter: Option<accesskit_winit::Adapter>,
    a11y_proxy: EventLoopProxy<UserEvent>,
    /// Last text-input-v3 state applied to winit. Edge-triggering this avoids
    /// emitting a protocol commit on every idle loop.
    ime_state: Option<ImeWindowState>,
    primary_frames_sent: u64,
    primary_frames_completed: u64,
    last_primary_frame_presented: u64,
    /// `None` waits for the next primary submission; `Some(sequence)` waits
    /// until that exact-or-newer frame is reported Presented by the worker.
    floating_source_clear_frames: std::collections::HashMap<String, Option<u64>>,
    /// Reverse ownership gate. A suspended native surface remains covered by
    /// its compositor-frozen final frame until one primary app frame with the
    /// reattached widget is reported Presented.
    floating_destination_present_frames: std::collections::HashMap<String, Option<u64>>,
    /// Maps one-finger taps onto the existing pointer event contract so every
    /// standard and app-local control gets touch without bespoke branches.
    touch_pointer: crate::interaction::TouchPointerEmulator,
    /// Synthetic mouse events must retain touch modality rather than making
    /// the host immediately classify them as a fine pointer.
    dispatching_touch_pointer: bool,
}

struct AudiencePresentationWindow {
    monitor: winit::monitor::MonitorHandle,
    window: Arc<Window>,
    render: Option<RenderThreadHandle>,
    render_init: Option<mpsc::Receiver<Result<RenderThreadHandle, String>>>,
}

/// Keep the audience bound to the same monitor object across hotplug list
/// reordering; indices are only a one-tick selection aid.
fn audience_monitor_still_selected<T: PartialEq>(
    current: Option<&T>,
    selected: Option<&T>,
) -> bool {
    current
        .zip(selected)
        .is_some_and(|(current, selected)| current == selected)
}

#[cfg(test)]
mod audience_monitor_identity_tests {
    use super::audience_monitor_still_selected;

    #[test]
    fn stable_identity_survives_monitor_enumeration_reordering() {
        let audience = "connector-2";
        let reordered = ["connector-2", "connector-1"];
        assert!(audience_monitor_still_selected(
            reordered.first(),
            Some(&audience)
        ));
        assert!(!audience_monitor_still_selected(
            reordered.get(1),
            Some(&audience)
        ));
    }
}

struct NativeFloatingWindow {
    id: String,
    token: String,
    desktop_rect: KurboRect,
    window: Arc<Window>,
    render: Option<RenderThreadHandle>,
    /// Device/surface creation is much slower than a frame on software and
    /// headless backends.  Keeping it off the winit thread lets the frozen
    /// compositor proxy continue following the pointer during handoff.
    render_init: Option<mpsc::Receiver<Result<RenderThreadHandle, String>>>,
    prepared_at: Instant,
    adopted: bool,
    suspended: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImeWindowState {
    purpose: winit::window::ImePurpose,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Snapshot widget damage while its draw-updated geometry still describes the
/// scene being submitted. Callers deliberately clear dirty flags afterwards.
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
fn draw_frame_content(
    app: &mut App,
    scene: &mut Scene,
    width: u32,
    height: u32,
    scale_factor: f32,
    t_draw_start: Option<std::time::Instant>,
) -> (Affine, KurboRect, u128) {
    app.refresh_control_action_availability();
    // Widget rects are logical; this transform rasterises at the physical
    // resolution, so the frame stays crisp at any fractional scale.
    let transform = crate::scale::composite_transform(f64::from(scale_factor));
    let (vp_w, vp_h) = crate::scale::logical_size((width, height), f64::from(scale_factor));
    let window_rect = KurboRect::new(0.0, 0.0, vp_w, vp_h);
    let prev_vp = app.presence_ctx.push_viewport(window_rect);
    // No GPU damage clip: vello allocates a fresh `target_texture` per
    // frame, so clipping out a static region would just paint background
    // instead of preserving the last frame.
    let __prof_draw = app.profiler.as_ref().map(|_| std::time::Instant::now());
    crate::widget::native_view_frame_start(app.root_widget.as_mut());
    if app.history_layout.is_none_or(|layout| layout.app_visible) {
        app.root_widget.draw_with_presence(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
            &app.permission_ctx,
        );
    }
    app.presence_ctx.pop_viewport(prev_vp);
    // Widgets whose visual extent escapes their layout box (an open
    // `Dropdown`, a popover) composite above every later-drawn sibling here.
    // No-op by default.
    if app.history_layout.is_none_or(|layout| layout.app_visible) {
        app.root_widget.draw_overlay(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
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
    if let Some(dialog) = app.close_admission.dialog.as_mut() {
        dialog.draw(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
    }
    if let Some(ap) = app.actions.action_palette.as_mut() {
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

fn draw_visible_tooltip(
    app: &mut App,
    scene: &mut Scene,
    transform: Affine,
    width: u32,
    height: u32,
    scale_factor: f32,
) {
    app.input.tooltip_host.tick(Instant::now());
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

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// Only active in `App::debug_mode` — draws each widget's layout rect and a
/// depth-cycled label over the frame just rendered.
fn draw_debug_outlines(app: &mut App, scene: &mut Scene, transform: Affine) {
    fn draw_debug_recursive(
        widget: &dyn Widget,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        depth: usize,
        cycle: &[Color; 3],
    ) {
        let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
            return;
        };
        let rect = widget.get_rect();
        let &[a, b, c] = cycle;
        let color = match depth.checked_rem(3).unwrap_or(0) {
            0 => a,
            1 => b,
            _ => c,
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            color,
            None,
            &rect,
        );
        let label = format!("{}: {}", widget.name(), depth);
        let text_y = (rect.y0 + 12.0).min(rect.y1);
        let bg_rect = KurboRect::new(rect.x0, rect.y0, rect.x0 + 100.0, rect.y0 + 14.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 180),
            None,
            &bg_rect,
        );
        text_renderer.draw_text(
            scene,
            &label,
            10.0,
            color,
            {
                #[allow(clippy::arithmetic_side_effects, reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow")]
                let label_transform = transform * Affine::translate((rect.x0 + 2.0, text_y));
                label_transform
            },
            None,
        );
        widget.for_each_child(&mut |child| {
            draw_debug_recursive(
                child,
                scene,
                transform,
                text_renderer,
                depth.saturating_add(1),
                cycle,
            );
        });
    }
    let debug_outline_cycle = [
        app.theme.tokens.colors.semantic.warning,
        app.theme.tokens.colors.semantic.info,
        app.theme.tokens.colors.semantic.success,
    ];
    draw_debug_recursive(
        app.root_widget.as_ref(),
        scene,
        transform,
        &mut app.text_renderer,
        0,
        &debug_outline_cycle,
    );
}

/// Split out of `AppHandler::handle_redraw_requested` (`clippy::too_many_lines`).
/// `None` means taffy layout computation failed and the caller must abandon
/// the frame; otherwise `(relayout_branch, compute_us, apply_us)` for the
/// perf log line.
#[allow(
    clippy::too_many_lines,
    reason = "one bounded solve/recompose/focus/mount lifecycle keeps ownership and final-frame ordering explicit"
)]
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
    let logical = crate::scale::logical_size((width, height), f64::from(scale_factor));
    if app.close_admission.dialog.is_some() {
        app.layout_close_dialog(logical);
        return Some((true, 0, 0));
    }
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
    let view = app.root_widget.view_state();
    // A pending structural rebuild means the old map may already contain
    // removed controls. Capture only from an admitted, unchanged topology.
    app.layout.structure_dirty |= app.root_widget.take_structure_dirty();
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
                app.root_widget.restore_view_state(view);
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
            }
            current_root = app
                .layout
                .root_node
                .ok_or(crate::widget::WidgetLayoutError::CompositionDidNotSettle)?;
        }
        if must_compute || rebuild {
            let start = std::time::Instant::now();
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
        let start = std::time::Instant::now();
        app.apply_history_presentation(current_root);
        t_apply_us = t_apply_us.saturating_add(start.elapsed().as_micros());
        let dirty = app.root_widget.take_structure_dirty();
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
    // NativeView measurement is independent of platform attachment. Mount only
    // the successfully settled tree; mount publishes its retained geometry.
    if (structure_rebuilt || recomposed)
        && let Some(host) = native_host
    {
        crate::widget::mount_native_views(app.root_widget.as_mut(), host);
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
                std::time::Duration::from_micros(u64::try_from(micros).unwrap_or(u64::MAX)),
            );
        }
    }
    Some((true, t_compute_us, t_apply_us))
}

fn create_window_render(
    window: &Arc<Window>,
    transparent: bool,
) -> Result<RenderThreadHandle, String> {
    create_window_render_prepared(window, transparent, None)
}

pub(crate) struct PreparedRenderer {
    context: RenderContext,
    device_id: usize,
    renderer: Renderer,
}

#[cfg(windows)]
pub(crate) type RendererPreparation = mpsc::Receiver<Result<PreparedRenderer, String>>;

/// Start only for a real native launch, before loading app state and fonts.
/// This owns no window/surface and can safely finish if construction fails:
/// dropping the receiving end drops the prepared GPU resources at send.
#[cfg(windows)]
pub(crate) fn prepare_native_renderer() -> Option<RendererPreparation> {
    if std::env::var_os("TILO_SERIAL_RENDER_INIT").is_some() {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    match std::thread::Builder::new()
        .name("native-render-prepare".into())
        .spawn(move || {
            let prepare = || {
                tilo_startup_metrics::mark("native-render-prepare-start");
                let mut context = RenderContext::new();
                let device_id = pollster::block_on(context.device(None))
                    .ok_or_else(|| "could not prepare a drawing device".to_string())?;
                let device = context
                    .devices
                    .get(device_id)
                    .ok_or_else(|| "prepared drawing device index out of range".to_string())?;
                let renderer = create_native_renderer(device)?;
                tilo_startup_metrics::mark("native-render-prepare-done");
                Ok(PreparedRenderer {
                    context,
                    device_id,
                    renderer,
                })
            };
            let _ = sender.send(prepare());
        }) {
        Ok(_) => Some(receiver),
        Err(error) => {
            tracing::warn!(%error, "could not prepare renderer in parallel");
            None
        }
    }
}

fn create_window_render_prepared(
    window: &Arc<Window>,
    transparent: bool,
    prepared: Option<PreparedRenderer>,
) -> Result<RenderThreadHandle, String> {
    tilo_startup_metrics::mark("window-render-context-start");
    let size = window.inner_size();
    let (mut render_cx, prepared_renderer) = match prepared {
        Some(prepared) => (
            prepared.context,
            Some((prepared.device_id, prepared.renderer)),
        ),
        None => (RenderContext::new(), None),
    };
    let mut surface = pollster::block_on(render_cx.create_surface(
        window.clone(),
        size.width.max(1),
        size.height.max(1),
        vello::wgpu::PresentMode::AutoVsync,
    ))
    .map_err(|e| format!("could not create a drawing surface: {e}"))?;
    if transparent {
        // `Auto` commonly resolves to Opaque on Wayland even when winit's
        // window was created transparent. Floating surfaces paint
        // premultiplied Vello output, so request the matching compositor mode
        // explicitly; otherwise the transparent gutter becomes a black box.
        surface.config.alpha_mode = vello::wgpu::CompositeAlphaMode::PreMultiplied;
    }
    let device_handle = render_cx
        .devices
        .get(surface.dev_id)
        .ok_or_else(|| "render device index out of range".to_string())?;
    // Flipping to the sRGB suffix here would panic on mismatch with the
    // format `create_surface` already chose. `configure` returns no
    // `Result`, so guard it like the worker's reconfigures do
    // (`render_thread::configure_surface_checked`) — an already-lost
    // surface would otherwise hit wgpu's panicking default handler.
    tilo_startup_metrics::mark("window-surface-validation-scope-start");
    let configure_scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    tilo_startup_metrics::mark("window-surface-checked-configure-start");
    surface
        .surface
        .configure(&device_handle.device, &surface.config);
    tilo_startup_metrics::mark("window-surface-checked-configure-done");
    if let Some(err) = pollster::block_on(configure_scope.pop()) {
        return Err(format!("could not configure the drawing surface: {err}"));
    }
    tilo_startup_metrics::mark("window-surface-validation-scope-done");
    // create_surface checks presentation support and selects another device
    // if necessary. Never use pipelines built for a different device.
    let renderer = match prepared_renderer {
        Some((device_id, renderer)) if device_id == surface.dev_id => renderer,
        _ => create_native_renderer(device_handle)?,
    };
    // After this, every GPU call happens on its render worker.
    let render = spawn_render_thread(render_cx, surface, renderer)?;
    tilo_startup_metrics::mark("window-render-context-done");
    Ok(render)
}

fn create_native_renderer(device_handle: &crate::gpu::DeviceHandle) -> Result<Renderer, String> {
    let device = &device_handle.device;
    tilo_startup_metrics::mark("window-vello-init-start");
    let renderer = Renderer::new(
        device,
        RendererOptions {
            // Diagnostic/low-end fallback: CPU coarse stages with the same
            // GPU fine raster. Kept opt-in until the benchmark proves it is
            // faster on a device; the default remains the GPU pipeline.
            use_cpu: std::env::var_os("QUICK_UI_VELLO_CPU").is_some(),
            // Every frame below renders with `AaConfig::Area`. Advertising
            // 8x/16x MSAA as well only compiles and retains shader variants
            // this framework never selects, increasing startup and GPU
            // memory pressure on exactly the low-end devices we target.
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .map_err(|e| format!("could not create the renderer: {e}"))?;
    tilo_startup_metrics::mark("window-vello-init-done");
    #[cfg(windows)]
    device_handle.finish_shader_cache();
    Ok(renderer)
}

fn begin_window_render_init(
    window: Arc<Window>,
    transparent: bool,
) -> Result<mpsc::Receiver<Result<RenderThreadHandle, String>>, String> {
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("floating-render-init".into())
        .spawn(move || {
            let result = create_window_render(&window, transparent);
            // The surface may have been permanently closed while a slow GPU
            // backend was still probing. In that case, stop the newly-created
            // render worker here so its surface is dropped before `window`.
            if let Err(mpsc::SendError(result)) = result_tx.send(result)
                && let Ok(mut render) = result
            {
                render.shutdown_and_join();
            }
        })
        .map_err(|error| format!("could not start floating renderer setup: {error}"))?;
    Ok(result_rx)
}

impl AppHandler {
    /// Called before showing the window on Windows, and from the first
    /// `RedrawRequested` elsewhere so GPU init doesn't starve Wayland's
    /// initial focus events. `Err` is returned, not panicked, since every step here can
    /// fail when the compositor exits mid-open.
    fn init_render(&mut self, window: &Arc<Window>) -> Result<(), String> {
        #[cfg(windows)]
        if let Some(preparation) = self.app.startup_renderer.take() {
            match preparation.recv() {
                Ok(Ok(prepared)) => {
                    self.render = Some(create_window_render_prepared(
                        window,
                        false,
                        Some(prepared),
                    )?);
                    return Ok(());
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "renderer preparation failed; selecting device for the actual surface")
                }
                Err(error) => {
                    tracing::warn!(%error, "renderer preparation disconnected; selecting device for the actual surface")
                }
            }
        }
        self.render = Some(create_window_render(window, false)?);
        Ok(())
    }

    /// Reconcile the root's optional audience projection with a *separate*
    /// native toplevel. A missing monitor list, a one-output session, or an
    /// invalid/self-target all leave this `None`; the root's normal fullscreen
    /// intent then supplies the single-output fallback.
    fn sync_audience_presentation(
        &mut self,
        event_loop: &ActiveEventLoop,
        presenter: &Arc<Window>,
    ) {
        let _entry = self.app.enter_wake_scope();
        let request = self.app.root_widget.audience_presentation_request();
        let monitors: Vec<winit::monitor::MonitorHandle> = presenter.available_monitors().collect();
        let primary_monitor = presenter.primary_monitor();
        let local_monitor = presenter
            .current_monitor()
            .or_else(|| primary_monitor.clone());
        let presenter_index = local_monitor.as_ref().and_then(|current| {
            monitors
                .iter()
                .position(|monitor| monitor == current)
                .and_then(|index| u32::try_from(index).ok())
        });
        let infos: Vec<crate::fullscreen::MonitorInfo> = monitors
            .iter()
            .enumerate()
            .map(|(index, monitor)| crate::fullscreen::MonitorInfo {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                width: monitor.size().width,
                height: monitor.size().height,
                primary: primary_monitor.as_ref() == Some(monitor),
                scale: monitor.scale_factor(),
            })
            .collect();
        let target = request.and_then(|request| {
            crate::fullscreen::select_audience_output(&infos, request.target, presenter_index)
        });

        if self.audience_presentation.as_ref().is_some_and(|audience| {
            audience_monitor_still_selected(
                target.and_then(|index| monitors.get(index as usize)),
                Some(&audience.monitor),
            )
        }) {
            return;
        }
        self.close_audience_presentation();
        let Some(index) = target else {
            return;
        };
        let Some(monitor) = monitors.get(index as usize).cloned() else {
            return;
        };

        let attributes = Window::default_attributes()
            .with_title("Audience presentation")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false);
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let package_id = std::env::var("TILO_PACKAGE_ID").ok();
            let app_id = resolve_app_id(
                self.app.window.app_id.as_deref().or(package_id.as_deref()),
                std::env::current_exe()
                    .ok()
                    .and_then(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .as_deref(),
            );
            // A role-like suffix keeps a compositor from mistaking the
            // audience output for a duplicate editor surface.
            attributes.with_name(format!("{app_id}.audience"), "")
        };
        let Ok(window) = event_loop.create_window(attributes) else {
            tracing::warn!(
                "could not create separate audience presentation window; using single-output fallback"
            );
            return;
        };
        let window = Arc::new(window);
        window.set_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone()))));
        window.set_visible(true);
        window.request_redraw();
        self.audience_presentation = Some(AudiencePresentationWindow {
            monitor,
            window,
            render: None,
            render_init: None,
        });
        self.app.root_widget.on_audience_presentation_changed(true);
        self.app.layout.layout_dirty = true;
        presenter.request_redraw();
    }

    /// Drop the render worker before the Window Arc, as required by wgpu's
    /// surface lifetime. This is deliberately idempotent: hotplug, explicit
    /// stop, and process teardown can race through the same state transition.
    fn close_audience_presentation(&mut self) {
        let _entry = self.app.enter_wake_scope();
        let Some(mut audience) = self.audience_presentation.take() else {
            return;
        };
        audience.window.set_visible(false);
        if let Some(mut render) = audience.render.take() {
            render.shutdown_and_join();
        }
        self.app.root_widget.on_audience_presentation_changed(false);
        self.app.layout.layout_dirty = true;
    }

    /// Paint one audience frame without running a second widget layout or
    /// synchronising a second document. The root is responsible for a pure
    /// audience projection of the same model used by the presenter surface.
    fn handle_audience_redraw(&mut self) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(audience) = self.audience_presentation.as_mut() else {
            return false;
        };
        if audience.render.is_none() {
            if audience.render_init.is_none() {
                match begin_window_render_init(audience.window.clone(), false) {
                    Ok(init) => audience.render_init = Some(init),
                    Err(error) => tracing::warn!(%error, "could not start audience renderer"),
                }
            }
            let Some(init) = audience.render_init.as_ref() else {
                return true;
            };
            match init.try_recv() {
                Ok(Ok(render)) => {
                    audience.render = Some(render);
                    audience.render_init = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not initialise audience renderer");
                    audience.render_init = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    audience.render_init = None;
                    return false;
                }
            }
        }
        let Some(render) = audience.render.as_mut() else {
            return false;
        };
        let mut followup = false;
        while let Ok(outcome) = render.frame_complete.try_recv() {
            match outcome {
                FrameOutcome::Presented => {}
                FrameOutcome::Reconfigured | FrameOutcome::Skipped => followup = true,
                FrameOutcome::SurfaceGone => return false,
            }
        }
        let mut scene = match render.scene_pool.try_recv() {
            Ok(scene) => scene,
            Err(mpsc::TryRecvError::Empty) => {
                if followup {
                    audience.window.request_redraw();
                }
                return true;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::warn!("audience render worker exited");
                return false;
            }
        };
        let Ok(mut damage_regions) = render.damage_pool.recv() else {
            tracing::warn!("audience render worker damage pool exited");
            return false;
        };
        damage_regions.clear();
        let size = audience.window.inner_size();
        let scale = audience.window.scale_factor();
        let rect = vello::kurbo::Rect::new(
            0.0,
            0.0,
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        );
        let drawn = self.app.root_widget.draw_audience_presentation(
            &mut scene,
            Affine::scale(scale),
            &mut self.app.text_renderer,
            &self.app.theme,
            rect,
        );
        if !drawn {
            return false;
        }
        if render
            .render_tx
            .send(RenderRequest::Frame {
                scene,
                width: size.width.max(1),
                height: size.height.max(1),
                base_color: self.app.theme.bg_app,
                damage_regions,
                full_redraw: true,
            })
            .is_err()
        {
            tracing::warn!(
                "audience render worker stopped; returning to the single-output fallback"
            );
            return false;
        }
        if followup {
            audience.window.request_redraw();
        }
        true
    }

    fn sync_ime(&mut self, window: &Window) {
        let _entry = self.app.enter_wake_scope();
        let next = self.app.ime_window_state();
        if next == self.ime_state {
            return;
        }
        match next {
            Some(intent) => {
                // Apply purpose before enabling: winit's Wayland backend
                // commits the enable immediately, so the first activation
                // must already carry the right content type. Cursor geometry
                // follows enable because winit intentionally ignores cursor
                // updates while IME is disabled.
                window.set_ime_purpose(intent.purpose);
                window.set_ime_allowed(true);
                window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(intent.x, intent.y),
                    winit::dpi::LogicalSize::new(intent.width, intent.height),
                );
            }
            None => window.set_ime_allowed(false),
        }
        self.ime_state = next;
    }

    fn create_native_floating_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: String,
        token: String,
        rect: KurboRect,
    ) -> Result<(), String> {
        let _entry = self.app.enter_wake_scope();
        if let Some(existing) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == id)
        {
            if existing.token != token {
                return Err("shell changed the token of a live floating surface".into());
            }
            if existing.desktop_rect.size() != rect.size() {
                let _ = existing
                    .window
                    .request_inner_size(native_floating_logical_size(rect));
            }
            existing.desktop_rect = rect;
            existing.prepared_at = Instant::now();
            existing.adopted = false;
            existing.suspended = false;
            existing.window.set_visible(true);
            existing.window.request_redraw();
            return Ok(());
        }
        let logical_size = native_floating_logical_size(rect);
        let attributes = Window::default_attributes()
            .with_title(format!("Floating · {id}"))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_visible(false)
            .with_inner_size(logical_size);
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes.with_name(
                format!(
                    "{}{}",
                    crate::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX,
                    token
                ),
                "",
            )
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("create floating surface window: {error}"))?,
        );
        window.set_visible(true);
        window.request_redraw();
        self.floating_windows.push(NativeFloatingWindow {
            id,
            token,
            desktop_rect: rect,
            window,
            render: None,
            render_init: None,
            prepared_at: Instant::now(),
            adopted: false,
            suspended: false,
        });
        Ok(())
    }

    fn suspend_native_floating_window(&mut self, id: &str) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(surface) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == id)
        else {
            return false;
        };
        surface.window.set_visible(false);
        surface.suspended = true;
        surface.adopted = false;
        true
    }

    fn close_native_floating_window(&mut self, id: &str) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.id == id)
        else {
            return false;
        };
        let mut surface = self.floating_windows.remove(index);
        surface.window.set_visible(false);
        if let Some(mut render) = surface.render.take() {
            render.shutdown_and_join();
        }
        true
    }

    fn apply_floating_host_messages(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let _entry = self.app.enter_wake_scope();
        let mut changed = false;
        for message in self.app.take_floating_host_messages() {
            match message {
                crate::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id,
                    token,
                    rect,
                    content_rect: _,
                    transfer_covered,
                    coordinate_offset: _,
                } => match crate::floating_surface::protocol_rect(rect) {
                    Ok(rect) => {
                        if transfer_covered {
                            self.floating_source_clear_frames.insert(id.clone(), None);
                            if let Some(primary) = self.window.as_ref() {
                                primary.request_redraw();
                            }
                        }
                        match self.create_native_floating_window(event_loop, id, token, rect) {
                            Ok(()) => changed = true,
                            Err(error) => {
                                tracing::warn!(%error, "could not prepare native floating surface");
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "shell sent invalid floating surface rect");
                    }
                },
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, rect } => {
                    if crate::floating_surface::protocol_rect(rect).is_ok()
                        && let Some(surface) = self
                            .floating_windows
                            .iter_mut()
                            .find(|surface| surface.id == id)
                    {
                        surface.adopted = true;
                        surface.window.request_redraw();
                        changed = true;
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id }
                | crate::floating_surface::FloatingSurfaceHostMessage::Closed { id } => {
                    self.floating_source_clear_frames.remove(&id);
                    self.floating_destination_present_frames.remove(&id);
                    changed |= self.close_native_floating_window(&id);
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Suspended { id } => {
                    self.floating_source_clear_frames.remove(&id);
                    self.floating_destination_present_frames
                        .insert(id.clone(), None);
                    changed |= self.suspend_native_floating_window(&id);
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::DesktopBounds { .. } => {}
            }
        }
        changed
    }

    /// A prepared surface that never commits must not strand its app-owned
    /// content half outside the primary window forever. Keep polling only
    /// while the first buffer is pending, then ask the shell to close the
    /// abandoned capability; `Closed` drives the ordinary animated return.
    fn recover_stalled_floating_windows(&mut self) -> bool {
        // A frozen compositor-side transfer frame keeps the surface visually
        // continuous while a cold GPU backend prepares. This deadline is now
        // a genuine failure bound, not a UX timer: ordinary adoption replaces
        // the proxy on its first committed buffer.
        // A cold secondary WGPU surface can spend several seconds enumerating
        // fallbacks on software/headless systems. The compositor's transfer
        // proxy is already covering every pixel, so returning at four seconds
        // caused a needless visible snap just before a viable backend could
        // finish. Ten seconds remains a bounded failure path without turning
        // ordinary slow device discovery into UX churn.
        const FIRST_BUFFER_DEADLINE: Duration = Duration::from_secs(10);
        let _entry = self.app.enter_wake_scope();
        let stalled = self
            .floating_windows
            .iter()
            .filter(|surface| {
                !surface.suspended
                    && !surface.adopted
                    && surface.prepared_at.elapsed() >= FIRST_BUFFER_DEADLINE
            })
            .map(|surface| surface.id.clone())
            .collect::<Vec<_>>();
        if stalled.is_empty() {
            return false;
        }
        for id in stalled {
            tracing::warn!(%id, "floating surface missed its first-buffer deadline; returning it to the app");
            if let Some(client) = self.app.floating_surface_client.as_mut() {
                let _ = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss {
                        id: id.clone(),
                    },
                );
            }
            self.close_native_floating_window(&id);
        }
        true
    }

    fn sync_native_floating_geometry(&mut self) -> bool {
        let _entry = self.app.enter_wake_scope();
        let presentations = self.app.root_widget.floating_surface_presentations();
        let mut changed = false;
        for surface in &mut self.floating_windows {
            if let Some(presentation) = presentations
                .iter()
                .find(|presentation| presentation.id == surface.id)
                && presentation.window_rect != surface.desktop_rect
            {
                if presentation.window_rect.size() != surface.desktop_rect.size() {
                    // App-owned live activities can change silhouette while
                    // detached: a call gains participant rows, navigation
                    // reveals another instruction, or accessibility text
                    // grows. Resize the real transparent buffer with the
                    // shared presentation rect so its soft chrome is never
                    // clipped inside the old native allocation.
                    let _ = surface
                        .window
                        .request_inner_size(native_floating_logical_size(presentation.window_rect));
                }
                surface.desktop_rect = presentation.window_rect;
                surface.window.request_redraw();
                changed = true;
            }
        }
        changed
    }

    fn handle_floating_redraw(&mut self, index: usize) -> bool {
        let _entry = self.app.enter_wake_scope();
        let (app, surfaces) = (&mut self.app, &mut self.floating_windows);
        let Some(surface) = surfaces.get_mut(index) else {
            return false;
        };
        if surface.suspended {
            return true;
        }
        if surface.render.is_none() {
            if surface.render_init.is_none() {
                match begin_window_render_init(surface.window.clone(), true) {
                    Ok(init) => surface.render_init = Some(init),
                    Err(error) => {
                        tracing::warn!(%error, id = %surface.id, "floating surface renderer failed");
                        return false;
                    }
                }
            }
            let Some(init) = surface.render_init.as_ref() else {
                return false;
            };
            match init.try_recv() {
                Ok(Ok(render)) => {
                    surface.render = Some(render);
                    surface.render_init = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, id = %surface.id, "floating surface renderer failed");
                    surface.render_init = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::warn!(id = %surface.id, "floating surface renderer setup stopped");
                    surface.render_init = None;
                    return false;
                }
            }
        }
        let Some(render) = surface.render.as_mut() else {
            return false;
        };
        let mut followup = false;
        while let Ok(outcome) = render.frame_complete.try_recv() {
            match outcome {
                FrameOutcome::Presented => {}
                FrameOutcome::Reconfigured | FrameOutcome::Skipped => followup = true,
                FrameOutcome::SurfaceGone => return false,
            }
        }
        let Ok(mut scene) = render.scene_pool.recv() else {
            return false;
        };
        let Ok(mut damage_regions) = render.damage_pool.recv() else {
            return false;
        };
        damage_regions.clear();
        // Every pooled scene is either newly seeded or reset by the worker
        // before recycling. Resetting it again here only rewrote all stream
        // lengths on every frame.
        let inner = surface.window.inner_size();
        let scale = surface.window.scale_factor();
        let transform = crate::scale::composite_transform(scale)
            .pre_translate((-surface.desktop_rect.x0, -surface.desktop_rect.y0).into());
        app.root_widget.draw_floating_surfaces(
            &mut scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
        if render
            .render_tx
            .send(RenderRequest::Frame {
                scene,
                width: inner.width.max(1),
                height: inner.height.max(1),
                base_color: Color::TRANSPARENT,
                damage_regions,
                full_redraw: true,
            })
            .is_err()
        {
            return false;
        }
        if followup {
            surface.window.request_redraw();
        }
        true
    }

    fn dispatch_floating_event(
        &mut self,
        window: &Arc<Window>,
        desktop_rect: KurboRect,
        event: WindowEvent,
    ) {
        let _entry = self.app.enter_wake_scope();
        let scale = window.scale_factor();
        let event = match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => WindowEvent::CursorMoved {
                device_id,
                position: PhysicalPosition::new(
                    position.x + desktop_rect.x0 * scale,
                    position.y + desktop_rect.y0 * scale,
                ),
            },
            WindowEvent::Touch(mut touch) => {
                touch.location = PhysicalPosition::new(
                    touch.location.x + desktop_rect.x0 * scale,
                    touch.location.y + desktop_rect.y0 * scale,
                );
                WindowEvent::Touch(touch)
            }
            other => other,
        };
        let app = &mut self.app;
        let Some(root_node) = app
            .history_layout
            .map(|layout| layout.app)
            .or(app.layout.root_node)
        else {
            return;
        };
        let mut handled = false;
        let mut focus_target = None;
        app.refresh_control_action_availability();
        app.root_widget.handle_event(
            &event,
            scale,
            &app.input.modifiers,
            &mut app.state.doc,
            &app.state.root,
            &mut handled,
            &mut focus_target,
            root_node,
            &app.layout.taffy,
        );
        // `handle_event` may change presentation-only resource declarations
        // without touching the CRDT (selected tab, open media, local mode).
        app.invalidate_widget_resource_cache();
        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
        }
        if handled {
            app.commit_action();
            app.needs_sync = true;
            app.layout.layout_dirty |= app.root_widget.is_layout_dirty();
            if let Some(primary) = self.window.as_ref() {
                primary.request_redraw();
            }
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the frame handshake shares render state, timing locals, and early exits"
    )]
    fn handle_redraw_requested(&mut self, event_loop: &ActiveEventLoop, window: &Arc<Window>) {
        // Capture only the initial frame attempts, not steady-state redraws.
        // An early return leaves its final marker absent rather than inventing
        // a completed stage. A later successful submission ends capture.
        let first_primary_frame = self.primary_frames_sent == 0;
        let mark_first_frame = |name| {
            if first_primary_frame {
                tilo_startup_metrics::mark(name);
            }
        };
        mark_first_frame("first-frame-redraw-start");
        let _entry = self.app.enter_wake_scope();
        if self.render.is_none() {
            tilo_startup_metrics::mark("first-redraw-init-render-start");
            if let Err(reason) = self.init_render(window) {
                // Retrying every redraw would only spin.
                crate::diag!("tilo: {reason}");
                event_loop.exit();
                return;
            }
            tilo_startup_metrics::mark("first-redraw-init-render-done");
        }
        let Some(render) = self.render.as_mut() else {
            return;
        };
        // A `Reconfigured` or `Skipped` previous frame re-requests a
        // redraw so the user doesn't see a stale image.
        let mut needs_followup_redraw = false;
        let mut surface_gone = false;
        while let Ok(outcome) = render.frame_complete.try_recv() {
            self.primary_frames_completed = self.primary_frames_completed.saturating_add(1);
            match outcome {
                FrameOutcome::Presented => {
                    self.last_primary_frame_presented = self.primary_frames_completed;
                }
                FrameOutcome::Reconfigured | FrameOutcome::Skipped => {
                    needs_followup_redraw = true;
                }
                FrameOutcome::SurfaceGone => {
                    surface_gone = true;
                }
            }
        }
        let cleared = completed_primary_frame_gates(
            &self.floating_source_clear_frames,
            self.last_primary_frame_presented,
        );
        for id in cleared {
            let sent =
                self.app
                    .floating_surface_client
                    .as_mut()
                    .is_some_and(|client| {
                        client
                    .send(&crate::floating_surface::FloatingSurfaceClientMessage::SourceCleared {
                        id: id.clone(),
                    })
                    .is_ok()
                    });
            if sent {
                self.floating_source_clear_frames.remove(&id);
            }
        }
        let destinations_presented = completed_primary_frame_gates(
            &self.floating_destination_present_frames,
            self.last_primary_frame_presented,
        );
        for id in destinations_presented {
            let sent = self
                .app
                .floating_surface_client
                .as_mut()
                .is_some_and(|client| {
                    client
                        .send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::DestinationPresented {
                                id: id.clone(),
                            },
                        )
                        .is_ok()
                });
            if sent {
                self.floating_destination_present_frames.remove(&id);
            }
        }
        if surface_gone {
            tracing::info!("drawing surface is gone; closing the window");
            event_loop.exit();
            return;
        }
        let app = &mut self.app;

        // Continue one bounded provider page per frame. Re-querying resets the
        // palette generation/cursors, so stale work cannot leak into a newer
        // query. A follow-up redraw is requested only while more pages exist.
        let (search_pending, search_announcement) =
            app.actions
                .action_palette
                .as_mut()
                .map_or((false, None), |palette| {
                    let progress = palette.advance_search_page();
                    (progress.pending, palette.take_search_announcement())
                });
        needs_followup_redraw |= search_pending;
        if let (Some(reader), Some(announcement)) = (
            app.accessibility.screen_reader.as_mut(),
            search_announcement,
        ) {
            reader.announce(announcement, tts::Priority::Notification, false);
        }

        let inner = window.inner_size();
        let width = inner.width;
        let height = inner.height;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scale factor is intentionally narrowed to f32 for the render pipeline; identical to today's output"
        )]
        let scale_factor = window.scale_factor() as f32;
        app.layout.last_frame_size = Some(inner);
        let mut floating_content_changed =
            app.layout.layout_dirty || app.layout.structure_dirty || app.needs_sync;
        let mut full_redraw_required = floating_content_changed;

        // Records first: if the root widget opened a different
        // record, sync against that record's document, not the
        // outgoing one's.
        mark_first_frame("first-frame-record-start");
        let record_changed = app.record_tick();
        mark_first_frame("first-frame-record-done");
        floating_content_changed |= record_changed;
        full_redraw_required |= record_changed;

        let __prof_frame_start = app.profiler.as_ref().map(|_| std::time::Instant::now());
        // Drain model notifications and advance update-driven widgets before
        // synchronization. A worker's `Wake` event reaches this pass without
        // requiring the app to own a proxy or poll inside its model.
        mark_first_frame("first-frame-update-start");
        if app.root_widget.update(&mut app.state.doc, &app.state.root) {
            floating_content_changed = true;
            full_redraw_required = true;
            app.invalidate_widget_resource_cache();
            app.needs_sync = true;
            app.layout.layout_dirty = true;
        }
        mark_first_frame("first-frame-update-done");
        mark_first_frame("first-frame-sync-start");
        if app.needs_sync {
            floating_content_changed = true;
            full_redraw_required = true;
            app.root_widget.sync(&mut app.state.doc, &app.state.root);
            app.invalidate_widget_resource_cache();
            app.needs_sync = false;
        }
        mark_first_frame("first-frame-sync-done");
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Sync, __prof_frame_start);
        }

        mark_first_frame("first-frame-live-regions-start");
        app.scan_live_regions();
        mark_first_frame("first-frame-live-regions-done");

        // E.g. a tab split inserting a new WindowGroup: rebuild the
        // taffy tree from scratch so new widgets get node ids the
        // layout pass below can see.
        mark_first_frame("first-frame-build-layout-start");
        app.layout.structure_dirty |= app.root_widget.take_structure_dirty();
        // Captured BEFORE `rebuild_structure` clears it — a fresh
        // taffy tree always needs a full solve regardless of the
        // scroll fast path below.
        let structure_rebuilt = app.layout.structure_dirty;
        floating_content_changed |= structure_rebuilt;
        let __prof_build = app.profiler.as_ref().map(|_| std::time::Instant::now());
        if app.layout.structure_dirty {
            app.rebuild_structure();
        }
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::BuildLayout, __prof_build);
        }

        mark_first_frame("first-frame-build-layout-done");

        let perf = perf_enabled();
        let t_frame_start = if perf {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let Some(root_node) = app.layout.root_node else {
            tracing::error!("root_node unset during a redraw; skipping frame");
            return;
        };
        mark_first_frame("first-frame-layout-start");
        let Some((t_relayout_branch, t_compute_us, t_apply_us)) = run_layout_pass(
            app,
            root_node,
            width,
            height,
            scale_factor,
            structure_rebuilt,
            Some(&crate::widget::NativeViewHost::live(
                window,
                app.input.platform,
            )),
        ) else {
            return;
        };
        mark_first_frame("first-frame-layout-done");
        full_redraw_required |= structure_rebuilt || t_relayout_branch;
        // Read before `draw_frame_content` drains widget dirty flags via
        // `mark_drawn`. Clean compositor/exposure redraws do not invalidate
        // independent native floating surfaces.
        floating_content_changed |= app.root_widget.needs_redraw() || app.history_needs_redraw();

        // Bounded CPU↔GPU backpressure belongs immediately before scene
        // encoding. Model update, sync and layout above can overlap the
        // worker presenting the previous frame instead of waiting for a
        // recycled encoder before doing any useful CPU work.
        let Ok(mut scene) = render.scene_pool.recv() else {
            tracing::error!("ui-render thread died; closing window");
            event_loop.exit();
            return;
        };
        let Ok(mut damage_regions) = render.damage_pool.recv() else {
            tracing::error!("ui-render damage pool died; closing window");
            event_loop.exit();
            return;
        };
        damage_regions.clear();
        full_redraw_required |= app.debug_mode
            || app.actions.action_palette.is_some()
            || app.input.tooltip_host.needs_frame()
            || app.input.tooltip_host.visible().is_some()
            || !app.presence_ctx.remote_cursors.is_empty()
            || app
                .profiler
                .as_ref()
                .is_some_and(super::profiling::FrameProfiler::hud_enabled);
        // The worker resets scenes before returning them to this pool; the
        // pre-seeded scenes are empty too.

        let t_draw_start = if perf {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let (transform, window_rect, t_draw_us) =
            draw_frame_content(app, &mut scene, width, height, scale_factor, t_draw_start);

        // A pointer that arrived on a desktop scope is the
        // compositor's to draw, and the helper skips it — see
        // `presence::PointerOwner`. Only the *painting* has one
        // owner; the shell threads this same context into its own
        // overlay too.
        let interaction_now = std::time::Instant::now();
        app.presence_ctx.prune_interactions(interaction_now);
        crate::presence::draw_remote_presence(
            &mut scene,
            transform,
            window_rect,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
        // Controls and named rows already publish their real accessibility
        // geometry. Reuse that exact focus/selection outline for semantic
        // action presence; this decorates the target without changing local
        // focus, selection, active tab, or scroll position.
        let visible_interactions = app.presence_ctx.visible_interactions(interaction_now);
        if !visible_interactions.is_empty() {
            app.refresh_semantic_tree();
        }
        let native_targets = visible_interactions
            .iter()
            .filter_map(|interaction| {
                let exact = match &interaction.anchor {
                    tilo_actions::InteractionAnchor::SemanticControl { id } => app
                        .semantic_tree()
                        .node(&crate::a11y_tree::SemanticId(id.clone()))
                        .and_then(|node| node.bounds),
                    tilo_actions::InteractionAnchor::ListItems { .. }
                    | tilo_actions::InteractionAnchor::Resource => {
                        let mut matches = app
                            .semantic_tree()
                            .iter()
                            .filter(|node| {
                                node.name.as_deref() == Some(interaction.resource.label.as_str())
                            })
                            .filter_map(|node| node.bounds);
                        let only = matches.next();
                        only.filter(|_| matches.next().is_none())
                    }
                    _ => None,
                }?;
                Some((interaction.clone(), exact))
            })
            .collect::<Vec<_>>();
        for (interaction, rect) in native_targets {
            let user = crate::permissions::UserId(interaction.actor.id);
            let label = app.presence_ctx.display_name(&user);
            app.presence_ctx.draw_peer_outline_labeled(
                &mut scene,
                &app.theme,
                &mut app.text_renderer,
                transform,
                &user,
                &label,
                rect,
                f64::from(app.theme.tokens.radii.sm),
            );
        }
        // Action/resource callers have no synthetic pointer. Their truthful
        // semantic lifecycle is painted as an app-level activity rail until a
        // domain widget projects the stable anchor into native geometry.
        app.presence_ctx.draw_interactions(
            &mut scene,
            transform,
            window_rect,
            &mut app.text_renderer,
            &app.theme,
            interaction_now,
        );

        // Folds in any `handle_event` time banked since last frame.
        if let (Some(p), Some(start)) = (app.profiler.as_mut(), __prof_frame_start) {
            p.commit_frame(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
        }

        if app.debug_mode {
            draw_debug_outlines(app, &mut scene, transform);
        }

        // Animated widgets update their visual bounds while drawing (for
        // example, an indeterminate progress segment records its old and new
        // positions there). Capture damage only after that work, but before
        // `mark_drawn` clears regular dirty flags. This makes the submitted
        // scene and its damage describe the same frame rather than trailing
        // animation by one frame.
        if !full_redraw_required {
            collect_submitted_widget_damage(app.root_widget.as_ref(), &mut damage_regions);
            let physical_viewport = KurboRect::new(0.0, 0.0, f64::from(width), f64::from(height));
            let scale = f64::from(scale_factor);
            for rect in &mut damage_regions {
                *rect = KurboRect::new(
                    rect.x0 * scale,
                    rect.y0 * scale,
                    rect.x1 * scale,
                    rect.y1 * scale,
                )
                .inflate(16.0 * scale, 16.0 * scale)
                .intersect(physical_viewport);
            }
            damage_regions.retain(|rect| !rect.is_zero_area());
        }
        // This must follow damage collection: widget `draw` may update
        // damage geometry and `mark_drawn` consumes persistent dirty flags.
        app.root_widget.mark_drawn();

        // `update_if_active` only builds + sends when an AT is
        // attached, so this is free on the common no-reader path.
        if let Some(adapter) = self.a11y_adapter.as_mut() {
            let app_ref: &mut App = app;
            let scale = f64::from(scale_factor);
            adapter.update_if_active(|| {
                crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale))
            });
        }

        // Painted last, after the frame timing was committed above,
        // so the HUD's own cost never enters its numbers.
        if app
            .profiler
            .as_ref()
            .is_some_and(super::profiling::FrameProfiler::hud_enabled)
        {
            let (hud_w, hud_h) =
                crate::scale::logical_size((width, height), f64::from(scale_factor));
            if let Some(p) = app.profiler.as_ref() {
                p.draw_hud(
                    &mut scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                    (hud_w, hud_h),
                );
            }
        }
        let req = RenderRequest::Frame {
            scene,
            width,
            height,
            base_color: app.theme.bg_app,
            damage_regions,
            full_redraw: full_redraw_required,
        };
        if render.render_tx.send(req).is_err() {
            tracing::error!("ui-render thread receiver closed; exiting");
            event_loop.exit();
            return;
        }
        mark_first_frame("first-frame-submitted");
        self.primary_frames_sent = self.primary_frames_sent.saturating_add(1);
        let submitted = self.primary_frames_sent;
        for required in self.floating_source_clear_frames.values_mut() {
            if required.is_none() {
                *required = Some(submitted);
            }
        }
        for required in self.floating_destination_present_frames.values_mut() {
            if required.is_none() {
                *required = Some(submitted);
            }
        }

        let t_send_us = t_frame_start.map_or(0, |t| t.elapsed().as_micros());
        if let Some(_start) = t_frame_start {
            // On a static frame this reads `shape=0us hit=N miss=0`.
            let (shape_hits, shape_misses, shape_ns) = app.text_renderer.take_shape_stats();
            crate::diag!(
                "perf frame: main_total={:>5}us layout={}({}+{}us) draw={:>5}us (shape={:>4}us hit={} miss={}) send_to_worker={:>5}us",
                t_send_us,
                if t_relayout_branch { "Y" } else { "n" },
                t_compute_us,
                t_apply_us,
                t_draw_us,
                shape_ns.checked_div(1000).unwrap_or(0),
                shape_hits,
                shape_misses,
                t_send_us,
            );
        }

        if needs_followup_redraw {
            window.request_redraw();
        }
        // The secondary surfaces paint the same app-owned state. A primary
        // frame therefore invalidates each of them exactly once; their own
        // presentation never recursively requests another primary frame.
        if floating_content_changed {
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
        if let Some(audience) = self.audience_presentation.as_ref() {
            audience.window.request_redraw();
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "input routing shares mutable state and must retain fixed precedence"
    )]
    fn handle_other_window_event(&mut self, window: &Arc<Window>, other: WindowEvent) {
        let _entry = self.app.enter_wake_scope();
        let event = other;
        if self.app.close_handle_event(&event, window.scale_factor()) {
            window.request_redraw();
            return;
        }
        let app = &mut self.app;
        let detected_interaction = if self.dispatching_touch_pointer {
            None
        } else {
            match &event {
                WindowEvent::Touch(_) => Some(app.input.interaction.with_touch_modality()),
                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. } => {
                    Some(app.input.interaction.with_pointer_modality())
                }
                WindowEvent::KeyboardInput {
                    event:
                        winit::event::KeyEvent {
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => Some(app.input.interaction.with_keyboard_modality()),
                _ => None,
            }
        };
        if let Some(environment) = detected_interaction {
            app.set_interaction_environment(environment);
        }
        let __prof_input = app.profiler.as_ref().map(|_| std::time::Instant::now());
        // RUST_LOG=ui::app=debug: pointer-move spam is intentionally
        // not throttled here — when the bug is "no clicks work", you
        // want to see whether moves arrive at all.
        match &event {
            WindowEvent::MouseInput { state, button, .. } => {
                tracing::debug!(?state, ?button, "App::window_event MouseInput");
            }
            WindowEvent::CursorMoved { position, .. } => {
                tracing::trace!(
                    x = position.x,
                    y = position.y,
                    "App::window_event CursorMoved"
                );
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                tracing::debug!(key = ?ke.logical_key, state = ?ke.state, "App::window_event KeyboardInput");
            }
            _ => {}
        }
        // `CursorMoved` is measured in this window's own content,
        // never the desktop — see the `pointer_space` field.
        if app.presence.broadcast_scope.is_some() {
            match &event {
                WindowEvent::CursorMoved { position, .. } => {
                    let now = Instant::now();
                    if pointer_broadcast_allowed(
                        app.presence.last_pointer_broadcast,
                        now,
                        Duration::from_millis(POINTER_BROADCAST_MIN_GAP_MS),
                    ) {
                        app.presence.last_pointer_broadcast = Some(now);
                        let where_it_is = app.local_pointer_key();
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "presence coordinates are intentionally narrowed to f32 on the wire; identical to today's output"
                        )]
                        app.broadcast_presence(
                            where_it_is,
                            PresencePayload::Pointer {
                                x: position.x as f32,
                                y: position.y as f32,
                            },
                        );
                    }
                }
                WindowEvent::CursorLeft { .. } => {
                    let where_it_was = app.local_pointer_key();
                    app.broadcast_presence(where_it_was, PresencePayload::Left);
                    app.presence.last_pointer_broadcast = None;
                }
                _ => {}
            }
        }

        let mut handled = false;
        let mut history_handled = matches!(event, WindowEvent::KeyboardInput { .. })
            && app
                .focused_node
                .is_some_and(|node| app.is_history_node(node));
        let mut focus_target = None;
        // A synthesized hover-clear may be handled by a control, but it is
        // presentation state and must not create an empty undo boundary.
        let mut is_undo_redo =
            self.dispatching_touch_pointer && matches!(event, WindowEvent::CursorMoved { .. });
        let mut floating_pointer_needs_redraw = false;

        // The palette owns the keyboard while open; Ctrl+K itself is
        // handled below, so let it through.
        if app.actions.action_palette.is_some() {
            let is_ctrl_k = matches!(
                &event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(c),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                    ..
                } if (c.as_str() == "k" || c.as_str() == "K")
                    && app
                        .input
                        .platform
                        .command_modifier_down(app.input.modifiers.state())
            );
            if !is_ctrl_k
                && matches!(event, WindowEvent::KeyboardInput { .. })
                && app.action_palette_handle_key(&event)
            {
                handled = true;
            }
        }

        // Runs FIRST so it wins over Tab/arrow handling.
        if app.handle_screen_reader_key(&event) {
            handled = true;
            is_undo_redo = true; // suppress an undo commit for a read cmd
        }

        let state_pre = if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
        ) {
            app.focused_state_snapshot()
        } else {
            None
        };

        let review_pre = if matches!(event, WindowEvent::KeyboardInput { .. }) {
            app.text_review_snapshot()
        } else {
            None
        };

        // Governs pointer click-through only; keyboard focus
        // containment is the separate `traps_focus()` concern (see
        // `keyboard_nav_allowed`).
        let modal_rect = app.root_widget.modal_capture();

        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            app.debug_mode = !app.debug_mode;
            handled = true;
        }

        if !handled
            && app
                .input
                .platform
                .command_modifier_down(app.input.modifiers.state())
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(ref c),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let shift = app.input.modifiers.state().shift_key();
            let history_request = crate::undo::shortcut(c, true, shift);
            if let Some(request) = history_request {
                if app.root_widget.handle_local_undo_request(request) {
                    app.needs_sync = true;
                    window.request_redraw();
                } else {
                    // Otherwise round-tripping document undo/redo loses caret
                    // position.
                    let current_meta = app.root_widget.get_state();
                    let current_focus_idx = app
                        .focused_node
                        .and_then(|id| app.focusable_order.iter().position(|x| *x == id));
                    let entry = if let Some(binding) = app
                        .focused_node
                        .and_then(|node| app.bindings.binding_for(node))
                    {
                        binding
                            .history_request(&mut app.state.doc, request)
                            .ok()
                            .flatten()
                    } else {
                        match request {
                            crate::undo::UndoRequest::Undo => app.state.undo_as(
                                Some(&app.collaboration.local_author),
                                current_focus_idx,
                                current_meta,
                            ),
                            crate::undo::UndoRequest::Redo => app.state.redo_as(
                                Some(&app.collaboration.local_author),
                                current_focus_idx,
                                current_meta,
                            ),
                        }
                    };
                    if let Some(entry) = entry {
                        if entry.instance_id.is_some() {
                            app.needs_sync = true;
                            app.layout.layout_dirty = true;
                        } else {
                            app.apply_undo_entry(entry);
                        }
                    }
                }
                is_undo_redo = true;
                handled = true;
            } else {
                match c.as_str() {
                    // A second Ctrl+K closes the palette.
                    "k" | "K" => {
                        if app.actions.action_palette.is_some() {
                            app.close_action_palette();
                        } else {
                            app.open_action_palette();
                        }
                        handled = true;
                    }
                    _ => {}
                }
            }
        }

        if !handled && app.handle_tab_navigation(&event, modal_rect) {
            handled = true;
        }

        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(logical_key),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let dir = match logical_key {
                winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                _ => None,
            };
            if let (Some(d), true, true) = (
                dir,
                app.input.modifiers.state().alt_key(),
                app.keyboard_nav_allowed(modal_rect),
            ) {
                app.navigate_focus(d);
                handled = true;
            }
        }

        // Two-finger swipe / edge pull; delivered via `on_nav_gesture`.
        if !handled && !self.dispatching_touch_pointer {
            use crate::gesture::GesturePhase;
            let map_phase = |p: winit::event::TouchPhase| match p {
                winit::event::TouchPhase::Started => GesturePhase::Begin,
                winit::event::TouchPhase::Moved => GesturePhase::Update,
                winit::event::TouchPhase::Ended => GesturePhase::End,
                winit::event::TouchPhase::Cancelled => GesturePhase::Cancel,
            };
            let nav = match &event {
                WindowEvent::MouseWheel { delta, phase, .. } => {
                    let (dx, dy, precise) = match delta {
                        winit::event::MouseScrollDelta::PixelDelta(p) => (p.x, p.y, true),
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            (f64::from(*x), f64::from(*y), false)
                        }
                    };
                    app.input
                        .nav_gestures
                        .on_scroll(dx, dy, map_phase(*phase), precise)
                }
                WindowEvent::PanGesture { delta, phase, .. } => app.input.nav_gestures.on_scroll(
                    f64::from(delta.x),
                    f64::from(delta.y),
                    map_phase(*phase),
                    true,
                ),
                WindowEvent::Touch(t) => {
                    let width = f64::from(window.inner_size().width);
                    app.input.nav_gestures.on_touch(
                        t.id,
                        t.location.x,
                        t.location.y,
                        map_phase(t.phase),
                        width,
                    )
                }
                _ => None,
            };
            if let Some(dir) = nav {
                let consumed =
                    app.root_widget
                        .on_nav_gesture(dir, &mut app.state.doc, &app.state.root);
                if consumed {
                    app.needs_sync = true;
                    window.request_redraw();
                    handled = true;
                }
            }
        }

        if !handled && app.layout.root_node.is_some() {
            app.refresh_control_action_availability();
            let scale_factor = window.scale_factor();
            let presentation_was_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            let floating_pointer_event = app.primary_floating_pointer_event(&event, scale_factor);
            let used_desktop_pointer_space = floating_pointer_event.is_some();
            let event_for_root = floating_pointer_event.as_ref().unwrap_or(&event);
            let was_mouse_click = matches!(
                event,
                WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
            );
            history_handled |= app.presentation_handle_event(
                event_for_root,
                scale_factor,
                &mut handled,
                &mut focus_target,
            );
            // Even a document-free interaction may switch which resources a
            // widget presents. Invalidate once at the mutable tree boundary;
            // all later consumers in this event/frame share the rebuilt cache.
            app.invalidate_widget_resource_cache();
            let presentation_is_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            floating_pointer_needs_redraw = matches!(event, WindowEvent::CursorMoved { .. })
                && (used_desktop_pointer_space
                    || presentation_was_floating != presentation_is_floating);
            if was_mouse_click {
                tracing::debug!(
                    handled,
                    needs_redraw = app.root_widget.needs_redraw(),
                    is_layout_dirty = app.root_widget.is_layout_dirty(),
                    "root_widget.handle_event(MouseInput Pressed) returned"
                );
            }
            // A press the modal didn't consume (the dimmed backdrop)
            // is still marked handled so it can't reach widgets
            // behind the scrim.
            if modal_rect.is_some()
                && matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: winit::event::ElementState::Pressed,
                        ..
                    } | WindowEvent::MouseInput {
                        state: winit::event::ElementState::Released,
                        ..
                    }
                )
            {
                handled = true;
            }
        }

        if !handled && app.activate_focused_control_key(&event) {
            handled = true;
        }

        // A focused range control consumes Left/Right; Up/Down and Tab
        // still move focus. Modifiers scale the step:
        //   plain  → one step       Shift → four steps (coarse)
        //   Ctrl   → jump to min/max (Left = min, Right = max)
        // Other widgets ignore the action, falling through to
        // directional navigation below.
        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(named),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            use crate::a11y::Action;
            let ctrl = app
                .input
                .platform
                .command_modifier_down(app.input.modifiers.state());
            let shift = app.input.modifiers.state().shift_key();
            // (action to repeat, how many times)
            let (action, times) = match named {
                winit::keyboard::NamedKey::ArrowLeft if ctrl => {
                    (Some(Action::SetValue("0".into())), 1)
                }
                winit::keyboard::NamedKey::ArrowRight if ctrl => {
                    (Some(Action::SetValue("100".into())), 1)
                }
                winit::keyboard::NamedKey::ArrowLeft => {
                    (Some(Action::Decrement), if shift { 4 } else { 1 })
                }
                winit::keyboard::NamedKey::ArrowRight => {
                    (Some(Action::Increment), if shift { 4 } else { 1 })
                }
                _ => (None, 0),
            };
            if let (Some(act), Some(fid)) = (action, app.focused_node) {
                let mut any = false;
                for _ in 0..times {
                    any |= app
                        .dispatch_node(fid, |widget, doc, root| {
                            widget.perform_action(&act, doc, root)
                        })
                        .unwrap_or(false);
                }
                if any {
                    handled = true;
                    app.needs_sync = true;
                    app.layout.layout_dirty = true;
                }
            }
        }

        if !handled
            && app.keyboard_nav_allowed(modal_rect)
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(logical_key),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let dir = match logical_key {
                winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                _ => None,
            };
            if let Some(d) = dir {
                app.navigate_focus(d);
                handled = true;
            }
        }

        if app.dispatch_control_invocations() != 0 {
            handled = true;
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        if let Some(address) = app.root_widget.take_action_palette_request()
            && app.open_action_palette_at(&address)
        {
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        app.broadcast_focus_presence();
        app.broadcast_location_presence();
        app.broadcast_view_presence();
        app.publish_action_surface();
        app.publish_continuation_metadata();
        app.publish_playback_handoff();

        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
            handled = true;
        }

        app.announce_state_change(state_pre);
        app.announce_text_review(review_pre);

        if handled {
            app.finalize_handled_native_event(&event, is_undo_redo, history_handled);
            app.needs_sync = true;
            // Gating every handled event on `needs_redraw()` is
            // wrong: container widgets can mutate their own
            // active-index state without setting a descendant leaf's
            // `dirty` flag, so recursive `needs_redraw` misses it.
            // Only pointer MOVES (the hot 60+ Hz stream) skip a
            // redraw when nothing's dirty.
            let is_pointer_move = matches!(&event, WindowEvent::CursorMoved { .. });
            if !is_pointer_move || app.root_widget.is_layout_dirty() {
                app.layout.layout_dirty = true;
            }
            if !is_pointer_move
                || floating_pointer_needs_redraw
                || app.root_widget.needs_redraw()
                || app.history_needs_redraw()
            {
                window.request_redraw();
            }
        }

        // Resolved on every `CursorMoved`, not just handled ones, so
        // the shape stays correct crossing into a widget that
        // doesn't flip its own hover.
        if let WindowEvent::CursorMoved { position, .. } = &event {
            let scale = window.scale_factor();
            let logical = Point::new(position.x / scale, position.y / scale);
            let desired = app.resolve_cursor(logical).unwrap_or(CursorIcon::Default);
            if app.input.current_cursor != desired {
                app.input.current_cursor = desired;
                window.set_cursor(desired);
            }
            // `on_pointer` keeps the dwell running over the same
            // region and restarts it on a new one.
            let target = app.tooltip_target_at(logical);
            if app.input.tooltip_host.on_pointer(target, Instant::now()) {
                window.request_redraw();
            }
        }

        if matches!(
            &event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::CursorLeft { .. }
        ) && app.input.tooltip_host.dismiss()
        {
            window.request_redraw();
        }

        if let (Some(start), Some(p)) = (__prof_input, app.profiler.as_mut()) {
            p.record(crate::profiling::Pass::HandleEvent, start.elapsed());
        }
    }
}

impl ApplicationHandler<UserEvent> for AppHandler {
    // SAFETY: the zeroed-DeviceId transmutes below are safe because winit's
    // DeviceId is opaque and the dispatch chain never inspects it (same
    // trick `shell::compositor::native` uses headless).
    #[allow(
        clippy::too_many_lines,
        reason = "winit requires the complete synthetic event dispatch at this callback boundary"
    )]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let _entry = self.app.enter_wake_scope();
        let event = match event {
            UserEvent::Wake(wake) => {
                if self.app.accept_wake(&wake)
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
                return;
            }
            other => other,
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        let window_id = window.id();
        match event {
            UserEvent::Hover { x, y } => {
                let ev = WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(x, y),
                };
                self.window_event(event_loop, window_id, ev);
            }
            UserEvent::Click { x, y } => {
                let moved = WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(x, y),
                };
                self.window_event(event_loop, window_id, moved);
                let press = WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                };
                self.window_event(event_loop, window_id, press);
                let release = WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Released,
                    button: MouseButton::Left,
                };
                self.window_event(event_loop, window_id, release);
            }
            UserEvent::Wheel { dx, dy } => {
                let ev = WindowEvent::MouseWheel {
                    device_id: unsafe { std::mem::zeroed() },
                    delta: winit::event::MouseScrollDelta::LineDelta(dx, dy),
                    phase: winit::event::TouchPhase::Moved,
                };
                self.window_event(event_loop, window_id, ev);
            }
            UserEvent::External(payload) => {
                let app = &mut self.app;
                app.root_widget
                    .on_external_event(&payload, &mut app.state.doc, &app.state.root);
                app.needs_sync = true;
                window.request_redraw();
            }
            UserEvent::Wake(_) => {}
            UserEvent::AccessKit(ak_event) => {
                use accesskit_winit::WindowEvent as AkWindowEvent;
                match ak_event {
                    AkWindowEvent::InitialTreeRequested => {
                        if let Some(adapter) = self.a11y_adapter.as_mut() {
                            let scale = window.scale_factor();
                            let app_ref: &mut App = &mut self.app;
                            adapter.update_if_active(|| {
                                crate::accesskit_bridge::native_tree_update(
                                    app_ref.build_a11y_tree_update(scale),
                                )
                            });
                        }
                        window.request_redraw();
                    }
                    AkWindowEvent::ActionRequested(req) => {
                        // `Some((x, y))` means "synthesise a click here".
                        if let Some(action) = crate::accesskit_bridge::accesskit_action_to_ours(
                            req.action,
                            req.data.as_ref(),
                        ) {
                            let click = self.app.dispatch_a11y_action(req.target_node, &action);
                            if let Some((x, y)) = click {
                                let scale = window.scale_factor();
                                let (px, py) = (x * scale, y * scale);
                                let moved = WindowEvent::CursorMoved {
                                    device_id: unsafe { std::mem::zeroed() },
                                    position: PhysicalPosition::new(px, py),
                                };
                                self.window_event(event_loop, window_id, moved);
                                let press = WindowEvent::MouseInput {
                                    device_id: unsafe { std::mem::zeroed() },
                                    state: ElementState::Pressed,
                                    button: MouseButton::Left,
                                };
                                self.window_event(event_loop, window_id, press);
                                let release = WindowEvent::MouseInput {
                                    device_id: unsafe { std::mem::zeroed() },
                                    state: ElementState::Released,
                                    button: MouseButton::Left,
                                };
                                self.window_event(event_loop, window_id, release);
                            }
                            window.request_redraw();
                        }
                    }
                    AkWindowEvent::AccessibilityDeactivated => {}
                }
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let _entry = self.app.enter_wake_scope();
        // Can fire more than once (suspend/resume on mobile).
        if self.window.is_some() {
            return;
        }
        // Required, not incidental: the compositor only accepts
        // `set_selection` from the connection that owns the focused surface.
        #[cfg(target_os = "linux")]
        {
            crate::clipboard::install_wayland_backend_from_display(event_loop);
        }
        // AccessKit's adapter asserts it's created while the window is
        // hidden, so build invisible, create the adapter, show.
        let title = resolve_window_title(self.app.window.title.as_deref());
        let icon = crate::window_icon::resolve();
        let window_attributes = Window::default_attributes()
            .with_title(title)
            .with_window_icon(icon.clone())
            // Draggable title bar on compositors with no server-side
            // decorations (e.g. GNOME/mutter); explicit though it's the
            // winit default, so the intent is unmistakable.
            .with_decorations(true)
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        // See `resolve_app_id`: a shared id collapses per-app compositor
        // lookups.
        #[cfg(target_os = "linux")]
        let window_attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let package_id = std::env::var("TILO_PACKAGE_ID").ok();
            let app_id = resolve_app_id(
                self.app.window.app_id.as_deref().or(package_id.as_deref()),
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .as_deref(),
            );
            window_attributes.with_name(app_id, "")
        };
        // `resumed` is a winit `ApplicationHandler` callback returning `()`,
        // so a `create_window` failure (compositor gone) has nowhere to
        // propagate to; there is no window to run without it either way.
        #[allow(
            clippy::unwrap_used,
            reason = "fixed () trait signature; window-creation failure at startup is unrecoverable"
        )]
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::ActiveEventLoopExtMacOS;
            if let Err(error) = event_loop.set_quit_request_window(Some(window.id())) {
                crate::diag!("tilo: cannot protect application Quit: {error}");
                event_loop.exit();
                return;
            }
        }
        tilo_startup_metrics::mark("primary-window-created");
        // Some backends only honour the icon post-creation; a no-op on
        // Wayland compositors that source it from the app_id instead.
        window.set_window_icon(icon);

        // Must be created before the window is shown.
        self.a11y_adapter = Some(accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            self.a11y_proxy.clone(),
        ));
        crate::widget::mount_native_views(
            self.app.root_widget.as_mut(),
            &crate::widget::NativeViewHost::live(&window, self.app.input.platform),
        );
        // Windows otherwise exposes an empty white client area while GPU
        // pipelines compile. Prepare those before revealing the window; the
        // normal visible-window redraw path still submits its first frame.
        #[cfg(target_os = "windows")]
        if let Err(reason) = self.init_render(&window) {
            crate::diag!("tilo: {reason}");
            event_loop.exit();
            return;
        }
        tilo_startup_metrics::mark("primary-window-show-start");
        window.set_visible(true);
        tilo_startup_metrics::mark("primary-window-show-done");

        self.app.layout.layout_dirty = true;
        // On Wayland GPU init is deferred to first `RedrawRequested`: the
        // ~50ms surface+device init here would block `resumed`'s seat
        // wiring — on mutter that intermittently left the toplevel
        // unfocused with dead input on first launch.
        window.request_redraw();
        tilo_startup_metrics::mark("primary-window-redraw-requested");
        crate::dnd::set_drag_window(&window);
        self.window = Some(window);
        tilo_startup_metrics::mark("primary-window-resumed-done");
    }

    /// BEFORE the `Window` arc is dropped — the worker's
    /// `RenderSurface<'static>` references it, and wgpu may panic if the
    /// surface outlives the window.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.app.close_wake_scope();
        let _entry = self.app.enter_wake_scope();
        // The Unix adapter owns an asynchronous AT-SPI provider.  Drop it
        // before the native views and window disappear, so external clients
        // receive the provider's normal unregister/teardown rather than being
        // left with object paths whose backing Wayland surface is already
        // gone.  This also makes the ordering explicit instead of relying on
        // AppHandler field-drop order.
        self.a11y_adapter = None;
        if !self.app.close_admission.admitted {
            self.app.autosave_flush();
        }
        self.app.close_native_content();
        self.close_audience_presentation();
        for surface in &mut self.floating_windows {
            if let Some(mut render) = surface.render.take() {
                render.shutdown_and_join();
            }
        }
        self.floating_windows.clear();
        if let Some(mut render) = self.render.take() {
            render.shutdown_and_join();
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "winit requires the complete per-window event dispatch at this callback boundary"
    )]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let _entry = self.app.enter_wake_scope();
        if self
            .audience_presentation
            .as_ref()
            .is_some_and(|audience| audience.window.id() == window_id)
        {
            let audience_window = self
                .audience_presentation
                .as_ref()
                .map(|audience| audience.window.clone());
            let Some(audience_window) = audience_window else {
                return;
            };
            match event {
                WindowEvent::CloseRequested => {
                    // Closing the audience is an explicit stop, unlike a
                    // temporary hotplug reconciliation which retains the
                    // presentation and falls back to the primary output.
                    self.app.root_widget.on_audience_presentation_closed();
                    self.close_audience_presentation();
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    audience_window.request_redraw();
                }
                WindowEvent::RedrawRequested => {
                    if !self.handle_audience_redraw() {
                        self.close_audience_presentation();
                        if let Some(primary) = self.window.as_ref() {
                            primary.request_redraw();
                        }
                    }
                }
                WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
                other => {
                    let handled = self.app.root_widget.handle_audience_presentation_event(
                        &other,
                        &mut self.app.state.doc,
                        &self.app.state.root,
                    );
                    if handled {
                        self.app.commit_action();
                        self.app.needs_sync = true;
                        self.app.layout.layout_dirty = true;
                        if let Some(primary) = self.window.as_ref() {
                            primary.request_redraw();
                        }
                        audience_window.request_redraw();
                    }
                }
            }
            return;
        }
        if let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.window.id() == window_id)
        {
            let Some(surface) = self.floating_windows.get(index) else {
                return;
            };
            let window = surface.window.clone();
            let desktop_rect = surface.desktop_rect;
            let surface_id = surface.id.clone();
            match event {
                WindowEvent::CloseRequested => {
                    if let Some(client) = self.app.floating_surface_client.as_mut() {
                        let _ = client.send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss {
                                id: surface_id.clone(),
                            },
                        );
                    }
                    self.close_native_floating_window(&surface_id);
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    window.request_redraw();
                }
                WindowEvent::RedrawRequested => {
                    if !self.handle_floating_redraw(index) {
                        let id = self
                            .floating_windows
                            .get(index)
                            .map(|surface| surface.id.clone());
                        if let Some(id) = id {
                            self.close_native_floating_window(&id);
                        }
                    }
                }
                WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
                other => self.dispatch_floating_event(&window, desktop_rect, other),
            }
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        // No-op when no AT is attached.
        if let Some(adapter) = self.a11y_adapter.as_mut() {
            adapter.process_event(&window, &event);
        }
        if let WindowEvent::Touch(touch) = &event {
            self.app
                .set_interaction_environment(self.app.input.interaction.with_touch_modality());
            let pointer_events = self.touch_pointer.events(touch);
            self.dispatching_touch_pointer = true;
            for pointer_event in pointer_events {
                let pointer_event = match pointer_event {
                    crate::interaction::TouchPointerEvent::Move(position) => {
                        WindowEvent::CursorMoved {
                            device_id: touch.device_id,
                            position,
                        }
                    }
                    crate::interaction::TouchPointerEvent::Press => WindowEvent::MouseInput {
                        device_id: touch.device_id,
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                    },
                    crate::interaction::TouchPointerEvent::Release => WindowEvent::MouseInput {
                        device_id: touch.device_id,
                        state: ElementState::Released,
                        button: MouseButton::Left,
                    },
                    crate::interaction::TouchPointerEvent::Scroll { delta, phase } => {
                        WindowEvent::MouseWheel {
                            device_id: touch.device_id,
                            delta: winit::event::MouseScrollDelta::PixelDelta(delta),
                            phase,
                        }
                    }
                };
                self.handle_other_window_event(&window, pointer_event);
            }
            self.dispatching_touch_pointer = false;
        }
        match event {
            WindowEvent::CloseRequested => {
                if self.app.admit_native_close_request() {
                    event_loop.exit();
                } else {
                    window.request_redraw();
                }
            }
            WindowEvent::Resized(_size) => {
                // Handled lazily on the worker via the next `RenderRequest`'s
                // dimensions; main only needs to drive a redraw.
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                // The render path reads `window.scale_factor()` each frame,
                // so we just need to invalidate layout and redraw.
                crate::widget::mount_native_views(
                    self.app.root_widget.as_mut(),
                    &crate::widget::NativeViewHost::live(&window, self.app.input.platform),
                );
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.handle_redraw_requested(event_loop, &window),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.app.input.modifiers = modifiers;
            }
            other => self.handle_other_window_event(&window, other),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "winit idle work must remain ordered within its single lifecycle callback"
    )]
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.service_close_admission();
        if self.app.finish_close_if_ready() {
            event_loop.exit();
            return;
        }

        let _entry = self.app.enter_wake_scope();
        let Some(window) = self.window.clone() else {
            return;
        };
        self.sync_ime(&window);

        if self.app.pump_floating_surface_service() {
            self.app.invalidate_widget_resource_cache();
            self.app.layout.layout_dirty = true;
            window.request_redraw();
        }
        if self.apply_floating_host_messages(event_loop) {
            self.app.invalidate_widget_resource_cache();
            window.request_redraw();
        }
        self.sync_native_floating_geometry();
        if self.recover_stalled_floating_windows() {
            window.request_redraw();
        }
        // `RedrawRequested` polls the asynchronous renderer setup. The shell
        // keeps showing its live transfer proxy in the meantime, so these
        // frames are cheap but essential: the first real buffer should take
        // ownership as soon as it is ready, without waiting for fresh input.
        for surface in &self.floating_windows {
            if !surface.suspended && !surface.adopted {
                surface.window.request_redraw();
            }
        }
        let awaiting_primary_first_present = primary_startup_pump_needed(
            self.primary_frames_sent,
            self.last_primary_frame_presented,
        );
        if awaiting_primary_first_present {
            window.request_redraw();
        }
        // This runs before the ordinary fullscreen reconciliation below. If a
        // real second output appears, the root is notified first and keeps
        // the local window as the presenter dashboard; if it disappears the
        // existing fullscreen path immediately becomes the safe fallback.
        self.sync_audience_presentation(event_loop, &window);
        if let Some(audience) = self.audience_presentation.as_ref()
            && audience.render_init.is_some()
        {
            // Poll the worker result without ever blocking the presenter
            // event loop; this is short-lived only while GPU setup runs.
            audience.window.request_redraw();
        }
        let app = &mut self.app;

        // Catches a compositor reconfigure that changes `inner_size()`
        // without delivering `Resized`.
        if let Some(prev) = app.layout.last_frame_size
            && window.inner_size() != prev
        {
            app.layout.layout_dirty = true;
            window.request_redraw();
        }
        if app.pump_collab_presence() {
            window.request_redraw();
        }
        if app.poll_directed_invocations() {
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }
        app.publish_playback_handoff();

        // Keep pumping frames while any "just typed" highlighter mark is
        // still fading, so the taper animates instead of freezing.
        let typing_now = Instant::now();
        app.presence_ctx.prune_typing(typing_now);
        let typing_active = app.presence_ctx.has_active_typing(typing_now);
        if typing_active {
            window.request_redraw();
        }

        if app.dispatch_control_invocations() != 0 {
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        if app.service_history() {
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        if app.root_widget.needs_commit() {
            app.commit_action();
            app.root_widget.on_commit();
            app.needs_sync = true;
            window.request_redraw();
        }

        // Level-triggered rather than hooked into individual mutations:
        // widgets write to the document from many places.
        if let Err(error) = app.autosave_tick() {
            crate::diag!("[app] component autosave tick refused: {error:?}");
        }

        if app.reconcile_fullscreen(&window) {
            window.request_redraw();
        }

        // Run both: Settings' open Appearance pane may intentionally override
        // the file-level base with its richer live accessibility intent.
        let configured_changed = app.reconcile_configured_theme();
        let appearance_changed = app.reconcile_appearance();
        if configured_changed || appearance_changed {
            app.needs_sync = true;
            window.request_redraw();
        }

        // The active page/view can change during `sync` (e.g. the initial
        // auto-select), which the post-event broadcast above wouldn't catch.
        app.broadcast_location_presence();
        app.broadcast_view_presence();

        // Run here too, not only `RedrawRequested`, since painting isn't
        // guaranteed — a background tab, or a first frame behind a shader
        // compile.
        if app.record_tick() {
            window.request_redraw();
        }

        // Without this a first peer with no input never re-syncs to seed its
        // doc after the grace window, so a late joiner diverges.
        if app.collaboration.manager.is_connected() && app.root_widget.needs_idle_sync() {
            app.needs_sync = true;
            window.request_redraw();
        }

        app.broadcast_doc_delta();

        // Flag drained by the draw handler, not here — just pump a redraw.
        if app.root_widget.take_structure_dirty() {
            app.layout.structure_dirty = true;
            window.request_redraw();
        }

        let scheduled_deadline = crate::close_admission::wake_deadline(
            app.root_widget.next_wake_deadline(),
            app.close_admission.save_waiting,
            Instant::now(),
        );
        let scheduled_due = scheduled_deadline.is_some_and(|deadline| deadline <= Instant::now());
        if scheduled_due {
            app.needs_sync = true;
            window.request_redraw();
        }

        // A hover tooltip is not motion, so it reveals even under
        // reduce-motion — requested independently of the animation pump.
        let tooltip_pending = app.input.tooltip_host.needs_frame();
        if tooltip_pending {
            window.request_redraw();
        }

        // Reduced motion lets the loop go idle since animated widgets
        // already paint their resting state.
        let needs_frame = app.root_widget.needs_animation_frame();
        if needs_frame && !app.theme.reduce_motion {
            app.needs_sync = true;
            // Forces `apply_layout` since an animation tick (e.g. momentum
            // fling) can reposition content without an input event.
            app.layout.layout_dirty = true;
            window.request_redraw();
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
            if let Some(audience) = self.audience_presentation.as_ref() {
                audience.window.request_redraw();
            }
        }

        // A background pump can run while its visible projection is stable.
        // Redrawing only on a changed snapshot avoids full-scene GPU work on
        // every otherwise-idle collaboration wake.
        if let Some(tick) = app.presence.frame_tick.as_mut()
            && tick(&mut app.presence_ctx)
        {
            window.request_redraw();
        }

        // `Wait` sleeps until real input. Visual work keeps a display-cadence
        // pump; an otherwise-idle collaboration socket gets a slower bounded
        // poll because its shovel does not yet own a winit wake proxy.
        let awaiting_floating_pixels = self
            .floating_windows
            .iter()
            .any(|surface| !surface.suspended && !surface.adopted);
        let visual_work =
            // A widget may keep reporting animation demand until it receives
            // another paint. Under reduced motion we intentionally suppress
            // that paint above, so counting the raw flag here would leave an
            // otherwise-idle app waking at 60 Hz forever.
            animation_pump_needed(needs_frame, app.theme.reduce_motion)
            || tooltip_pending
            || typing_active
            || awaiting_floating_pixels
            || awaiting_primary_first_present
            || scheduled_due;
        match idle_pump_deadline(
            Instant::now(),
            visual_work,
            app.collaboration.manager.is_connected(),
            scheduled_deadline,
        ) {
            IdlePumpCadence::Deadline(deadline) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
            IdlePumpCadence::Wait => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

#[cfg(test)]
mod window_title_tests {
    use super::*;

    #[test]
    fn humanize_ident_titlecases_and_splits() {
        assert_eq!(humanize_ident("notes"), "Notes");
        assert_eq!(humanize_ident("file-explorer"), "File Explorer");
        assert_eq!(humanize_ident("word-processor"), "Word Processor");
        assert_eq!(humanize_ident("db_client"), "Db Client");
        assert_eq!(humanize_ident("notes"), "Notes");
        assert_eq!(humanize_ident(""), "");
    }

    #[test]
    fn explicit_title_wins_over_everything() {
        let t = resolve_title_from(
            Some("My Doc — Notes"),
            Some("Notes"),
            Some("so.tilo.notes"),
            Some("notes"),
        );
        assert_eq!(t, "My Doc — Notes");
    }

    #[test]
    fn app_name_env_resolves_to_the_manifest_name() {
        let t = resolve_title_from(None, Some("Notes"), Some("so.tilo.notes"), Some("notes"));
        assert_eq!(t, "Notes");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn package_id_falls_back_to_humanized_last_segment() {
        let t = resolve_title_from(
            None,
            None,
            Some("so.tilo.file-explorer"),
            Some("file-explorer"),
        );
        assert_eq!(t, "File Explorer");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn binary_name_is_the_dev_run_fallback() {
        let t = resolve_title_from(None, None, None, Some("terminal"));
        assert_eq!(t, "Terminal");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn blank_candidates_are_skipped_not_used() {
        let t = resolve_title_from(Some("  "), Some(""), Some("so.tilo.notes"), Some("notes"));
        assert_eq!(t, "Notes");
    }

    #[test]
    fn generic_default_is_tilo_app_never_quickui_app() {
        let t = resolve_title_from(None, None, None, None);
        assert_eq!(t, "Tilo App");
        assert_ne!(t, "QuickUI App");
    }

    #[test]
    fn the_app_id_is_never_the_generic_one_when_anything_identifies_the_app() {
        assert_eq!(
            resolve_app_id(Some("so.tilo.file-explorer"), Some("file-explorer")),
            "so.tilo.file-explorer",
            "the package id is the identity everything else is keyed on"
        );
        assert_eq!(
            resolve_app_id(None, Some("file-explorer")),
            "file-explorer",
            "outside a sandbox the binary still names this app, and it \
             matches the trailing-component convention package ids use"
        );
        assert_eq!(
            resolve_app_id(Some("   "), Some("terminal")),
            "terminal",
            "a blank package id is no package id"
        );
        assert_eq!(
            resolve_app_id(None, None),
            "org.tilo.app",
            "the generic id survives only when there is nothing to say"
        );
        assert_ne!(resolve_app_id(None, Some("file-explorer")), "org.tilo.app");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn env_path_end_to_end_prefers_app_name() {
        let prev_name = std::env::var_os("TILO_APP_NAME");
        let prev_id = std::env::var_os("TILO_PACKAGE_ID");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe {
            std::env::set_var("TILO_APP_NAME", "Weather");
            std::env::set_var("TILO_PACKAGE_ID", "so.tilo.weather");
        }
        let resolved = resolve_window_title(None);
        // Restore before asserting so a failure doesn't leak env.
        unsafe {
            match prev_name {
                Some(v) => std::env::set_var("TILO_APP_NAME", v),
                None => std::env::remove_var("TILO_APP_NAME"),
            }
            match prev_id {
                Some(v) => std::env::set_var("TILO_PACKAGE_ID", v),
                None => std::env::remove_var("TILO_PACKAGE_ID"),
            }
        }
        assert_eq!(resolved, "Weather");
        assert_ne!(resolved, "QuickUI App");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn env_path_falls_back_to_package_id_without_app_name() {
        let prev_name = std::env::var_os("TILO_APP_NAME");
        let prev_id = std::env::var_os("TILO_PACKAGE_ID");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe {
            std::env::remove_var("TILO_APP_NAME");
            std::env::set_var("TILO_PACKAGE_ID", "so.tilo.calculator");
        }
        let resolved = resolve_window_title(None);
        unsafe {
            match prev_name {
                Some(v) => std::env::set_var("TILO_APP_NAME", v),
                None => std::env::remove_var("TILO_APP_NAME"),
            }
            match prev_id {
                Some(v) => std::env::set_var("TILO_PACKAGE_ID", v),
                None => std::env::remove_var("TILO_PACKAGE_ID"),
            }
        }
        assert_eq!(resolved, "Calculator");
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "tests assert exact assigned values without floating-point computation"
)]
mod pointer_broadcast_tests {
    use super::*;

    #[test]
    fn pointer_broadcast_allowed_first_call_always_passes() {
        let now = Instant::now();
        assert!(pointer_broadcast_allowed(
            None,
            now,
            Duration::from_millis(33)
        ));
    }

    #[test]
    fn pointer_broadcast_allowed_blocks_inside_gap() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(10);
        assert!(!pointer_broadcast_allowed(
            Some(t0),
            t1,
            Duration::from_millis(33),
        ));
    }

    #[test]
    fn pointer_broadcast_allowed_passes_at_and_past_gap() {
        // Exactly at the gap boundary is allowed — strict `>` would
        // accumulate single-ms drift under a sawtooth wakeup schedule.
        let t0 = Instant::now();
        assert!(pointer_broadcast_allowed(
            Some(t0),
            t0 + Duration::from_millis(33),
            Duration::from_millis(33),
        ));
        assert!(pointer_broadcast_allowed(
            Some(t0),
            t0 + Duration::from_millis(50),
            Duration::from_millis(33),
        ));
    }

    #[test]
    fn set_presence_broadcast_scope_resets_throttle() {
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        app.presence.last_pointer_broadcast = Some(Instant::now());
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace { id: "home".into() }));
        assert!(
            app.presence.last_pointer_broadcast.is_none(),
            "scope change must clear the throttle so the next move broadcasts immediately"
        );
        assert_eq!(
            app.presence_broadcast_scope(),
            Some(&PresenceKey::Workspace { id: "home".into() })
        );
    }

    /// Guards against `commit_action` serialising the whole document (which
    /// advances `AutoCommit::save`'s cursor and leaves `broadcast_doc_delta`
    /// nothing to send).
    #[test]
    fn a_commit_leaves_the_edit_for_the_incremental_broadcast() {
        use automerge::transaction::Transactable;

        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        // Drain the baseline so what the assertion sees is this edit alone.
        let _ = app.state.save_incremental();

        app.state_mut_for_test()
            .doc
            .put(&automerge::ROOT, "body", "the quarterly report")
            .expect("put on the root object");
        app.commit_action();

        assert!(
            !app.state.save_incremental().is_empty(),
            "the committed edit must survive as an incremental delta; an \
             intervening full save() would have advanced the save cursor and \
             left broadcast_doc_delta with nothing to send"
        );
    }

    /// The scope is what sends `Arrived`, so opt-in would mean an app that
    /// forgets to set it never receives state written before it opened.
    #[test]
    #[serial_test::serial]
    fn an_app_in_a_workspace_publishes_presence_without_being_asked() {
        let room = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
        // SAFETY: serialised against the sibling env test below.
        unsafe { std::env::set_var("TILO_WORKSPACE_ID", room) };
        let app = App::new(State::new(), Box::new(crate::widget::Text::new("t")));
        assert_eq!(
            app.presence_broadcast_scope(),
            Some(&PresenceKey::Workspace {
                id: room.to_string()
            }),
            "a workspace in the environment is a scope, with no app-side call"
        );
        unsafe { std::env::remove_var("TILO_WORKSPACE_ID") };
    }

    /// The nil cluster addresses nobody, so no room means nothing published.
    #[test]
    #[serial_test::serial]
    fn an_app_with_no_workspace_publishes_nothing() {
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("TILO_WORKSPACE_ID");
            std::env::remove_var("QUICK_CLUSTER_ID");
        }
        let app = App::new(State::new(), Box::new(crate::widget::Text::new("t")));
        assert!(app.presence_broadcast_scope().is_none());
    }

    #[test]
    fn setting_scope_announces_arrival() {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_captured = Arc::clone(&captured);
        app.set_presence_broadcast_sink(move |k, p| {
            sink_captured.lock().unwrap().push((k.clone(), p.clone()));
        });
        let key = PresenceKey::Workspace { id: "home".into() };
        app.set_presence_broadcast_scope(Some(key.clone()));
        // Clone out of the guard so the lock is released before the assertion,
        // rather than held across it.
        let observed = captured.lock().unwrap().clone();
        assert!(
            observed
                .iter()
                .any(|(k, p)| *k == key && matches!(p, PresencePayload::Arrived)),
            "setting a scope must announce Arrival, got {observed:?}"
        );
    }

    #[test]
    fn document_snapshot_merges_not_replaces() {
        use automerge::transaction::Transactable;
        use automerge::{ObjType, ROOT, ReadDoc};
        let mut base = automerge::AutoCommit::new();
        let m = base.put_object(&ROOT, "m", ObjType::Map).unwrap();
        base.put(&m, "x", 1).unwrap();
        let base_bytes = base.save();

        let app_state = State::load(&base_bytes).unwrap();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(app_state, root);
        {
            let doc = &mut app.state_mut_for_test().doc;
            let Ok(Some((automerge::Value::Object(ObjType::Map), m))) = doc.get(&ROOT, "m") else {
                panic!("m exists")
            };
            doc.put(&m, "a", 10).unwrap();
        }

        let mut peer = automerge::AutoCommit::load(&base_bytes).unwrap();
        let Ok(Some((automerge::Value::Object(ObjType::Map), pm))) = peer.get(&ROOT, "m") else {
            panic!("m exists")
        };
        peer.put(&pm, "b", 20).unwrap();
        let incoming = peer.save();

        let envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(incoming)
                .encode()
                .unwrap(),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([0; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            0,
        );
        app.apply_signed_document_envelope(&envelope, None);

        // A replace would drop "a".
        let doc = &mut app.state_mut_for_test().doc;
        let Ok(Some((automerge::Value::Object(ObjType::Map), m))) = doc.get(&ROOT, "m") else {
            panic!("m exists")
        };
        assert!(
            doc.get(&m, "a").unwrap().is_some(),
            "local edit survives merge"
        );
        assert!(doc.get(&m, "b").unwrap().is_some(), "remote edit merged in");
        assert!(doc.get(&m, "x").unwrap().is_some(), "base value present");
    }

    #[test]
    fn presence_broadcast_sink_receives_both_key_and_payload() {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_captured = Arc::clone(&captured);
        app.set_presence_broadcast_sink(move |k, p| {
            sink_captured.lock().unwrap().push((k.clone(), p.clone()));
        });
        let key = PresenceKey::Workspace { id: "home".into() };
        app.broadcast_presence(key.clone(), PresencePayload::Pointer { x: 5.0, y: 7.0 });
        let observed = captured.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].0, key);
        match observed[0].1 {
            PresencePayload::Pointer { x, y } => {
                assert_eq!(x, 5.0);
                assert_eq!(y, 7.0);
            }
            ref other => panic!("expected Pointer payload, got {other:?}"),
        }
    }

    #[test]
    fn clear_presence_broadcast_sink_drops_callback() {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let sink_count = Arc::clone(&count);
        app.set_presence_broadcast_sink(move |_, _| {
            *sink_count.lock().unwrap() += 1;
        });
        app.broadcast_presence(
            PresenceKey::Workspace { id: "x".into() },
            PresencePayload::Arrived,
        );
        app.clear_presence_broadcast_sink();
        app.broadcast_presence(
            PresenceKey::Workspace { id: "x".into() },
            PresencePayload::Arrived,
        );
        assert_eq!(
            *count.lock().unwrap(),
            1,
            "sink fires before clear, never after"
        );
    }
}

#[cfg(test)]
mod rebuild_focus_tests {
    use super::*;
    use crate::widget::{Text, TextInput};
    use automerge::{AutoCommit, ObjId};
    use vello::kurbo::Rect;

    /// Recreates its children on every `build_layout`: the focused
    /// `TextInput` is a brand-new instance after rebuild, so its caret
    /// resets unless the App restores it via `get_state`/`set_state`.
    struct RebuildHarness {
        show_status: bool,
        request_reset: bool,
        children: Vec<Box<dyn Widget>>,
        rect: Rect,
    }

    impl RebuildHarness {
        fn new() -> Self {
            Self {
                show_status: false,
                request_reset: false,
                children: Vec::new(),
                rect: Rect::ZERO,
            }
        }

        fn rebuild_children(&mut self) {
            let mut kids: Vec<Box<dyn Widget>> = Vec::new();
            // `with_initial_value` parks the caret at the END ("hello" -> 5),
            // so a restored caret of 3 is distinguishable from both the
            // reset-to-0 bug and the new instance's own default.
            kids.push(Box::new(TextInput::local().with_initial_value("hello")));
            if self.show_status {
                kids.push(Box::new(Text::new("status")));
            }
            self.children = kids;
        }
    }

    impl Widget for RebuildHarness {
        fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
            for c in &mut self.children {
                c.sync(state, path);
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.rebuild_children();
            let child_nodes: Vec<NodeId> = self
                .children
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &child_nodes)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let layout = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
                origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
                origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
            );
            if let Ok(child_nodes) = taffy.children(node) {
                for (c, &cn) in self.children.iter_mut().zip(child_nodes.iter()) {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
        }
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _r: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _n: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.children.iter().map(Box::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.children
                .iter_mut()
                .map(|c| c.as_mut() as &mut (dyn Widget + '_))
                .collect()
        }
        fn on_external_event(&mut self, payload: &str, _s: &mut AutoCommit, _r: &ObjId) {
            match payload {
                "show_status" => self.show_status = true,
                "reset" => self.request_reset = true,
                _ => {}
            }
        }
        fn take_focus_reset(&mut self) -> bool {
            std::mem::take(&mut self.request_reset)
        }
        fn name(&self) -> &'static str {
            "RebuildHarness"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    fn caret(app: &App) -> Option<String> {
        let id = app.focused_node?;
        let w = *app.layout.node_map.get(&id)?;
        unsafe { (*w).get_state() }
    }

    #[test]
    fn caret_preserved_across_structure_rebuild() {
        let mut app =
            App::new_with_controlled_inputs(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.expect("input focused at startup");
        unsafe {
            (*(*app.layout.node_map.get(&id).unwrap())).set_state("3");
        }
        assert_eq!(caret(&app).as_deref(), Some("3"));

        // Triggers a structural rebuild that recreates the input as a fresh
        // instance whose own caret default is 5.
        let mut tmp = AutoCommit::new();
        app.root_widget
            .on_external_event("show_status", &mut tmp, &automerge::ROOT);
        app.rebuild_structure();

        assert!(app.focused_node.is_some(), "focus preserved across rebuild");
        assert_eq!(
            caret(&app).as_deref(),
            Some("3"),
            "caret offset must be preserved across the rebuild"
        );
        assert_eq!(
            app.root_widget.children().len(),
            2,
            "status sibling appeared"
        );
        assert_eq!(app.focusable_order.len(), 1, "status is non-focusable");
    }

    #[test]
    fn focus_reset_does_not_restore_caret() {
        let mut app =
            App::new_with_controlled_inputs(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.unwrap();
        unsafe {
            (*(*app.layout.node_map.get(&id).unwrap())).set_state("3");
        }

        let mut tmp = AutoCommit::new();
        app.root_widget
            .on_external_event("reset", &mut tmp, &automerge::ROOT);
        app.rebuild_structure();

        assert_eq!(
            caret(&app).as_deref(),
            Some("5"),
            "focus_reset must not paste the old caret onto the new field"
        );
    }
}

/// Mutating events should route through this so denied interactions
/// short-circuit before the widget ever observes them.
pub fn dispatch_gated_event<W: crate::widget::Widget + ?Sized>(
    ctx: &crate::permissions::PermissionContext,
    widget: &W,
    dispatch: impl FnOnce(),
) -> bool {
    if let Some(scope) = widget.permission_scope()
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
mod screen_reader_tests {
    use super::*;
    use crate::screen_reader::ScreenReader;
    use crate::widget::{Button, Widget};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use tts::{NullSpeaker, Speaker, Utterance};

    #[derive(Clone, Default)]
    struct SharedRecorder(Rc<RefCell<NullSpeaker>>);
    impl Speaker for SharedRecorder {
        fn speak_utterance(&mut self, u: Utterance) {
            self.0.borrow_mut().speak_utterance(u);
        }
        fn stop(&mut self) {
            self.0.borrow_mut().stop();
        }
        fn set_rate(&mut self, r: f32) {
            self.0.borrow_mut().set_rate(r);
        }
        fn set_voice(&mut self, v: &str) {
            self.0.borrow_mut().set_voice(v);
        }
        fn is_speaking(&self) -> bool {
            self.0.borrow().is_speaking()
        }
        fn backend_name(&self) -> &'static str {
            "shared-null"
        }
    }
    // SAFETY: test-only, single-threaded, so the !Send Rc is fine.
    #[allow(
        clippy::non_send_fields_in_send_ty,
        reason = "the Rc<RefCell<_>> is deliberately asserted Send above — this test double \
                  never actually crosses a thread"
    )]
    unsafe impl Send for SharedRecorder {}

    struct TwoButtons {
        kids: Vec<Box<dyn Widget>>,
    }
    impl TwoButtons {
        fn new() -> Self {
            Self {
                kids: vec![
                    Box::new(Button::new("a", "Save", None)),
                    Box::new(Button::new("b", "Cancel", None)),
                ],
            }
        }
    }
    impl Widget for TwoButtons {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            for c in &mut self.kids {
                c.sync(s, p);
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let kids: Vec<_> = self
                .kids
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &kids)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event_ctx(
            &mut self,
            _event: &WindowEvent,
            _ctx: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.kids.iter().map(Box::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.kids
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect()
        }
        fn name(&self) -> &'static str {
            "two-buttons"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
    }

    fn app_with_reader() -> (App, SharedRecorder) {
        let rec = SharedRecorder::default();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(TwoButtons::new()));
        app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
        (app, rec)
    }

    #[test]
    fn autofocus_narrates_the_focused_widget() {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert_eq!(rec.0.borrow().spoken(), &["Save, button".to_string()]);
    }

    #[test]
    fn directional_focus_change_narrates_new_widget() {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        let second = app.focusable_order[1];
        let ak_id = accesskit::NodeId(u64::from(second) + 1);
        app.dispatch_a11y_action(ak_id, &crate::a11y::Action::Focus);
        let spoken = rec.0.borrow();
        assert_eq!(
            spoken.spoken(),
            &["Save, button".to_string(), "Cancel, button".to_string()],
            "each focus change narrates the newly focused control"
        );
    }

    #[test]
    fn disabling_reader_silences_and_stops() {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert!(app.screen_reader_enabled());
        app.set_screen_reader_enabled(false);
        assert!(!app.screen_reader_enabled());
        assert!(
            rec.0.borrow().stop_count() >= 1,
            "disabling stops in-flight speech"
        );
        let before = rec.0.borrow().spoken().len();
        let second = app.focusable_order[1];
        let ak_id = accesskit::NodeId(u64::from(second) + 1);
        app.dispatch_a11y_action(ak_id, &crate::a11y::Action::Focus);
        assert_eq!(
            rec.0.borrow().spoken().len(),
            before,
            "no narration after the reader is disabled"
        );
    }

    #[test]
    fn reader_disabled_by_default_unless_env() {
        let mut app = App::new(State::new(), Box::new(TwoButtons::new()));
        app.autofocus_first = true;
        app.init_layout();
        assert!(
            !app.screen_reader_enabled(),
            "the reader is opt-in; default-off builds are unaffected"
        );
    }

    #[test]
    fn screen_reader_key_layer_is_noop_when_disabled() {
        // winit's KeyEvent has no public constructor; the disabled-guard is
        // the first line of the method, so a non-keyboard event exercises it.
        let mut app = App::new(State::new(), Box::new(TwoButtons::new()));
        app.init_layout();
        assert!(!app.screen_reader_enabled());
        let ev = WindowEvent::CursorLeft {
            device_id: unsafe { std::mem::zeroed() },
        };
        assert!(
            !app.handle_screen_reader_key(&ev),
            "reader-off App must never consume an event in the SR key layer"
        );
    }

    #[test]
    fn screen_reader_key_layer_ignores_non_keyboard_when_enabled() {
        let (mut app, _rec) = app_with_reader();
        app.init_layout();
        assert!(app.screen_reader_enabled());
        let ev = WindowEvent::CursorLeft {
            device_id: unsafe { std::mem::zeroed() },
        };
        assert!(!app.handle_screen_reader_key(&ev));
    }

    // ----- Live regions -----

    /// One live region whose spoken value is mutable between frames.
    struct LiveWidget {
        name: String,
        value: Rc<RefCell<String>>,
        politeness: crate::a11y::Politeness,
    }
    impl LiveWidget {
        fn new(name: &str, politeness: crate::a11y::Politeness) -> Self {
            Self {
                name: name.to_string(),
                value: Rc::new(RefCell::new(String::new())),
                politeness,
            }
        }
    }
    impl Widget for LiveWidget {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event_ctx(
            &mut self,
            _event: &WindowEvent,
            _ctx: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![]
        }
        fn name(&self) -> &'static str {
            "live-widget"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            let v = self.value.borrow().clone();
            let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_name(self.name.clone())
                .with_live(self.politeness);
            if !v.is_empty() {
                node = node.with_value(v);
            }
            Some(node)
        }
    }

    fn app_with_reader_over(root: Box<dyn Widget>) -> (App, SharedRecorder) {
        let rec = SharedRecorder::default();
        let mut app = App::new_with_controlled_inputs(State::new(), root);
        app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
        app.init_layout();
        (app, rec)
    }

    #[test]
    fn live_region_change_announces_without_moving_focus() {
        let live = LiveWidget::new("Notifications", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        let focus_before = app.focused_node;

        // Seed the snapshot, then forget it so we assert purely on the change.
        app.scan_live_regions();
        rec.0.borrow_mut().reset();

        *val.borrow_mut() = "File saved".to_string();
        app.scan_live_regions();
        assert_eq!(
            rec.0.borrow().last(),
            Some("Notifications, group, File saved"),
            "the changed live text is announced (name, role, value)"
        );
        assert_eq!(
            app.focused_node, focus_before,
            "live region must not move focus"
        );

        rec.0.borrow_mut().reset();
        app.scan_live_regions();
        assert!(
            rec.0.borrow().spoken().is_empty(),
            "unchanged live text is not re-announced"
        );
    }

    #[test]
    fn assertive_live_region_uses_important_interrupting_priority() {
        let live = LiveWidget::new("Errors", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        *val.borrow_mut() = "Permission denied".to_string();
        app.scan_live_regions();
        let recref = rec.0.borrow();
        let last = recref
            .utterances()
            .last()
            .expect("an utterance was recorded");
        assert_eq!(last.priority, tts::Priority::Important);
        assert!(last.interrupt, "assertive live regions interrupt");
    }

    #[test]
    fn polite_live_region_uses_notification_non_interrupting_priority() {
        let live = LiveWidget::new("Status", crate::a11y::Politeness::Polite);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        *val.borrow_mut() = "Uploading, 40%".to_string();
        app.scan_live_regions();
        let recref = rec.0.borrow();
        let last = recref
            .utterances()
            .last()
            .expect("an utterance was recorded");
        assert_eq!(last.priority, tts::Priority::Notification);
        assert!(!last.interrupt, "polite live regions do not interrupt");
    }

    #[test]
    fn live_region_scan_is_noop_when_reader_off() {
        let live = LiveWidget::new("Notifications", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let mut app = App::new(State::new(), Box::new(live));
        app.init_layout();
        assert!(!app.screen_reader_enabled());
        *val.borrow_mut() = "anything".to_string();
        app.scan_live_regions();
    }

    // ----- Text review (caret movement over existing content) -----

    fn app_with_focused_input(initial: &str) -> (App, SharedRecorder, NodeId) {
        let rec = SharedRecorder::default();
        let input = crate::widget::TextInput::local().with_initial_value(initial);
        let mut app = App::new(State::new(), Box::new(input));
        app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
        app.autofocus_first = true;
        app.init_layout();
        let id = app.focused_node.expect("the input is focused at startup");
        (app, rec, id)
    }

    #[test]
    fn caret_move_one_char_announces_the_character() {
        let (mut app, rec, id) = app_with_focused_input("abc");
        let pre = app.text_review_snapshot();
        assert!(pre.is_some(), "focused text input yields a review snapshot");
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        unsafe {
            (*(*app.layout.node_map.get(&id).unwrap())).set_state("2");
        }
        app.announce_text_review(pre);
        assert_eq!(
            rec.0.borrow().last(),
            Some("c"),
            "single-step move echoes the char"
        );
    }

    #[test]
    fn caret_jump_to_start_announces_the_word() {
        let (mut app, rec, id) = app_with_focused_input("hello world");
        let pre = app.text_review_snapshot();
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        unsafe {
            (*(*app.layout.node_map.get(&id).unwrap())).set_state("0");
        }
        app.announce_text_review(pre);
        assert_eq!(
            rec.0.borrow().last(),
            Some("hello"),
            "a long jump echoes the word"
        );
    }

    #[test]
    fn no_caret_move_announces_nothing() {
        let (mut app, rec, _id) = app_with_focused_input("abc");
        let pre = app.text_review_snapshot();
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        app.announce_text_review(pre);
        assert!(
            rec.0.borrow().spoken().is_empty(),
            "no move, no announcement"
        );
    }

    #[test]
    fn text_review_snapshot_is_none_for_non_text_widget() {
        let (mut app, _rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert!(app.focused_node.is_some());
        assert!(
            app.text_review_snapshot().is_none(),
            "a button has no caret to review"
        );
    }
}

/// The modal's open flag is a state change (no structural rebuild), so node
/// ids stay stable and `refresh_focus_order` re-evaluates the trap.
#[cfg(test)]
mod focus_trap_tests {
    use super::*;
    use crate::widget::modal_dialog::ModalDialog;
    use crate::widget::{Button, Widget};
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::rc::Rc;

    struct TwoKids {
        kids: Vec<Box<dyn Widget>>,
    }
    impl TwoKids {
        fn new() -> Self {
            Self {
                kids: vec![
                    Box::new(Button::new("ok", "OK", None)),
                    Box::new(Button::new("cancel", "Cancel", None)),
                ],
            }
        }
    }
    impl Widget for TwoKids {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            for c in &mut self.kids {
                c.sync(s, p);
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let kids: Vec<_> = self
                .kids
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &kids)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event_ctx(
            &mut self,
            _event: &WindowEvent,
            _ctx: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.kids.iter().map(Box::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.kids
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect()
        }
        fn name(&self) -> &'static str {
            "two-kids"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
    }

    /// A shared `want_open` cell flips the modal's state in `sync`, modelling
    /// a host that opens/closes without rebuilding the widget tree.
    struct ModalHost {
        outer: Vec<Box<dyn Widget>>,
        modal: ModalDialog<TwoKids>,
        want_open: Rc<Cell<bool>>,
    }
    impl ModalHost {
        fn new(want_open: Rc<Cell<bool>>) -> Self {
            Self {
                outer: vec![
                    Box::new(Button::new("a", "Open", None)),
                    Box::new(Button::new("b", "Other", None)),
                ],
                modal: ModalDialog::new("Confirm", TwoKids::new()),
                want_open,
            }
        }
    }
    impl Widget for ModalHost {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            self.modal.set_open(self.want_open.get());
            for c in &mut self.outer {
                c.sync(s, p);
            }
            self.modal.sync(s, p);
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.modal.set_open(self.want_open.get());
            let mut kids: Vec<_> = self
                .outer
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            kids.push(self.modal.build_layout(taffy, theme, node_map));
            let node = taffy
                .new_with_children(taffy::Style::default(), &kids)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event_ctx(
            &mut self,
            _event: &WindowEvent,
            _ctx: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            let mut v: Vec<&dyn Widget> = self.outer.iter().map(Box::as_ref).collect();
            v.push(&self.modal);
            v
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            let mut v: Vec<&mut (dyn Widget + '_)> = self
                .outer
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect();
            v.push(&mut self.modal as &mut (dyn Widget + '_));
            v
        }
        fn name(&self) -> &'static str {
            "modal-host"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
    }

    /// Built closed; flipping the cell + calling `refresh_focus_order`
    /// models the per-frame re-evaluation.
    fn host_app() -> (App, Rc<Cell<bool>>) {
        let cell = Rc::new(Cell::new(false));
        let app =
            App::new_with_controlled_inputs(State::new(), Box::new(ModalHost::new(cell.clone())));
        (app, cell)
    }

    #[test]
    fn closed_modal_does_not_trap_focus() {
        let (mut app, _open) = host_app();
        app.init_layout();
        assert!(
            !app.accessibility.focus_trap_active,
            "no trap while the modal is closed"
        );
        assert_eq!(
            app.focusable_order.len(),
            2,
            "closed modal controls stay out of the background focus order"
        );
    }

    #[test]
    fn open_modal_confines_focus_order_to_its_subtree() {
        let (mut app, open) = host_app();
        app.init_layout();
        let full = app.focusable_order.len();

        open.set(true);
        app.refresh_focus_order();

        assert!(
            app.accessibility.focus_trap_active,
            "an open modal traps focus"
        );
        assert_eq!(
            app.focusable_order.len(),
            2,
            "focus is confined to the modal's two buttons (was {full})"
        );
        assert!(
            app.focused_node
                .is_some_and(|id| app.focusable_order.contains(&id)),
            "focus moves into the modal when it opens"
        );
    }

    #[test]
    fn tab_cycles_within_the_trapped_modal_via_the_real_handler() {
        let (mut app, open) = host_app();
        app.init_layout();
        open.set(true);
        app.refresh_focus_order();
        assert!(app.accessibility.focus_trap_active);
        assert_eq!(app.focusable_order.len(), 2);
        let modal_set: std::collections::HashSet<_> = app.focusable_order.iter().copied().collect();
        let first = app.focusable_order[0];
        let second = app.focusable_order[1];

        // Must be allowed EVEN while a modal captures the pointer, or the
        // dialog's buttons would be unreachable by keyboard.
        assert!(
            app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 10.0, 10.0))),
            "an active focus trap allows Tab to run (scoped to the modal)"
        );

        app.set_focused_node(Some(first));
        assert!(app.focus_tab(false), "Tab moves focus");
        assert_eq!(
            app.focused_node,
            Some(second),
            "Tab advances within the modal"
        );
        assert!(modal_set.contains(&second));
        assert!(app.focus_tab(false));
        assert_eq!(
            app.focused_node,
            Some(first),
            "Tab from the last wraps to the first"
        );
        assert!(app.focus_tab(true));
        assert_eq!(
            app.focused_node,
            Some(second),
            "Shift+Tab from the first wraps to the last"
        );
        assert!(
            modal_set.contains(&app.focused_node.unwrap()),
            "Shift+Tab stays inside the modal subtree"
        );
    }

    #[test]
    fn pointer_capture_without_a_trap_suppresses_app_tab() {
        // A pointer-capturing, non-trapping surface (a context menu) must
        // switch off the App's own Tab/arrow navigation.
        let (mut app, _open) = host_app();
        app.init_layout();
        assert!(!app.accessibility.focus_trap_active);
        assert!(
            app.keyboard_nav_allowed(None),
            "with no modal, Tab runs normally"
        );
        assert!(
            !app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 10.0, 10.0))),
            "a pointer-capturing surface with no focus trap suppresses App-level Tab"
        );
    }

    #[test]
    fn closing_modal_restores_prior_focus() {
        let (mut app, open) = host_app();
        app.init_layout();
        let outer = app.focusable_order[0];
        app.set_focused_node(Some(outer));

        open.set(true);
        app.refresh_focus_order();
        assert!(app.accessibility.focus_trap_active);
        assert_ne!(app.focused_node, Some(outer), "focus moved into the modal");

        open.set(false);
        app.refresh_focus_order();
        assert!(
            !app.accessibility.focus_trap_active,
            "trap released when the modal closes"
        );
        assert_eq!(
            app.focused_node,
            Some(outer),
            "focus is restored to where it was before the modal opened"
        );
    }

    #[test]
    fn modal_exposes_dialog_role_with_title_as_name() {
        use crate::a11y::Role;
        let m = ModalDialog::new("Delete file?", TwoKids::new()).open(true);
        let node = Widget::accessibility(&m).expect("an open dialog is in the tree");
        assert_eq!(node.role, Role::Dialog);
        assert_eq!(node.name.as_deref(), Some("Delete file?"));
        assert!(node.state.modal, "an open modal traps focus → modal flag");
        assert!(!node.state.hidden);
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact preference values are part of the test contract"
)]
mod low_vision_tests {
    use super::*;
    use crate::a11y::contrast_ratio;

    fn app() -> App {
        App::new(State::new(), Box::new(crate::widget::Text::new("t")))
    }

    #[test]
    fn defaults_are_off() {
        let a = app();
        assert!(!a.high_contrast());
        assert!(!a.reduce_motion());
        assert_eq!(a.text_scale(), 1.0);
        assert!(!a.theme.reduce_motion);
        assert_eq!(a.theme.text_scale, 1.0);
    }

    #[test]
    fn toggle_high_contrast_swaps_the_theme() {
        let mut a = app();
        let plain_ratio = contrast_ratio(a.theme.text_primary, a.theme.bg_app);

        let now_on = a.toggle_high_contrast();
        assert!(now_on);
        assert!(a.high_contrast());
        let hc_ratio = contrast_ratio(a.theme.text_primary, a.theme.bg_app);
        assert!(
            hc_ratio > plain_ratio,
            "high contrast must raise fg/bg contrast ({hc_ratio:.1} vs {plain_ratio:.1})",
        );
        assert_eq!(a.theme.name, "High Contrast");

        let now_off = a.toggle_high_contrast();
        assert!(!now_off);
        assert!(!a.high_contrast());
        let restored = contrast_ratio(a.theme.text_primary, a.theme.bg_app);
        assert!(
            (restored - plain_ratio).abs() < 1e-6,
            "off restores base colours"
        );
    }

    #[test]
    fn text_scale_setter_scales_the_theme_type_size() {
        let mut a = app();
        let base = a.theme.tokens.typography.scale.base;
        a.set_text_scale(2.0);
        assert_eq!(a.text_scale(), 2.0);
        assert!((a.theme.tokens.typography.scale.base - base * 2.0).abs() < 1e-3);
        // A scale change must re-run layout so the larger text doesn't clip.
        assert!(a.layout.layout_dirty);
    }

    #[test]
    fn text_scale_survives_a_high_contrast_toggle() {
        let mut a = app();
        a.set_text_scale(1.5);
        let hc_base = Theme::high_contrast().tokens.typography.scale.base;
        a.set_high_contrast(true);
        assert_eq!(
            a.text_scale(),
            1.5,
            "scale preference is preserved across the swap"
        );
        assert!((a.theme.tokens.typography.scale.base - hc_base * 1.5).abs() < 1e-3);
        assert_eq!(a.theme.name, "High Contrast");
    }

    #[test]
    fn reduce_motion_setter_threads_onto_the_theme() {
        let mut a = app();
        assert!(!a.theme.reduce_motion);
        a.set_reduce_motion(true);
        assert!(a.reduce_motion());
        assert!(
            a.theme.reduce_motion,
            "the flag reaches the theme widgets draw against"
        );
        a.set_reduce_motion(false);
        assert!(!a.theme.reduce_motion);
    }

    #[test]
    fn set_theme_keeps_low_vision_preferences() {
        let mut a = app();
        a.set_text_scale(1.25);
        a.set_reduce_motion(true);
        a.set_high_contrast(true);
        // Swap the *base* theme — the a11y layer must re-apply on top.
        a.set_theme(Theme::light());
        assert!(a.theme.reduce_motion);
        assert_eq!(a.text_scale(), 1.25);
        // High contrast still wins over the freshly-set light base.
        assert_eq!(a.theme.name, "High Contrast");
    }

    #[test]
    fn ui_scale_setter_grows_spacing_radii_and_type_together() {
        let mut a = app();
        let base_gap = a.theme.tokens.spacing.s3;
        let base_radius = a.theme.tokens.radii.md;
        let base_type = a.theme.tokens.typography.scale.base;
        a.set_ui_scale(1.5);
        assert_eq!(a.ui_scale(), 1.5);
        assert!(
            (a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-3,
            "spacing grows"
        );
        assert!(
            (a.theme.tokens.radii.md - base_radius * 1.5).abs() < 1e-3,
            "radii grow"
        );
        assert!(
            (a.theme.tokens.typography.scale.base - base_type * 1.5).abs() < 1e-3,
            "type grows"
        );
        // A UI-scale change must re-run layout so the whole interface re-flows.
        assert!(a.layout.layout_dirty);
    }

    #[test]
    fn ui_scale_and_text_scale_compose_on_the_live_theme() {
        let mut a = app();
        let base_type = a.theme.tokens.typography.scale.base;
        let base_gap = a.theme.tokens.spacing.s3;
        a.set_ui_scale(1.25);
        a.set_text_scale(1.2);
        assert_eq!(a.ui_scale(), 1.25);
        assert_eq!(a.text_scale(), 1.2);
        // Type picks up BOTH factors; spacing only the UI factor.
        assert!((a.theme.tokens.typography.scale.base - base_type * 1.25 * 1.2).abs() < 1e-2);
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-2);
    }

    #[test]
    fn ui_scale_survives_a_high_contrast_toggle() {
        let mut a = app();
        a.set_ui_scale(1.5);
        let hc_gap = Theme::high_contrast().tokens.spacing.s3;
        a.set_high_contrast(true);
        assert_eq!(
            a.ui_scale(),
            1.5,
            "UI-scale preference is preserved across the swap"
        );
        assert!((a.theme.tokens.spacing.s3 - hc_gap * 1.5).abs() < 1e-2);
        assert_eq!(a.theme.name, "High Contrast");
    }

    struct AppearanceRoot {
        intent: crate::theme::AppearanceIntent,
    }

    impl Widget for AppearanceRoot {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _p: &crate::presence::PresenceCtx,
        ) {
        }
        fn handle_event_ctx(
            &mut self,
            _event: &WindowEvent,
            _ctx: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
            Some(self.intent.clone())
        }
        fn name(&self) -> &'static str {
            "AppearanceRoot"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
    }

    fn intent(
        theme: Theme,
        ui_scale: f32,
        text_scale: f32,
        high_contrast: bool,
    ) -> crate::theme::AppearanceIntent {
        crate::theme::AppearanceIntent {
            base_theme: theme,
            high_contrast,
            reduce_motion: false,
            text_scale,
            ui_scale,
        }
    }

    #[test]
    fn appearance_reconcile_off_is_a_noop() {
        let root = AppearanceRoot {
            intent: intent(Theme::nord(), 1.5, 1.3, true),
        };
        let mut a = App::new(State::new(), Box::new(root));
        assert!(!a.reconcile_appearance());
        assert_eq!(a.theme.name, "Dark");
        assert_eq!(a.ui_scale(), 1.0);
    }

    #[test]
    fn appearance_reconcile_applies_scale_theme_and_contrast_live() {
        let base_gap = Theme::high_contrast().tokens.spacing.s3;
        let base_type = Theme::high_contrast().tokens.typography.scale.base;
        // Nord base + high_contrast=true → high-contrast track wins; ui 1.5, text 1.2.
        let root = AppearanceRoot {
            intent: intent(Theme::nord(), 1.5, 1.2, true),
        };
        let mut a = App::new(State::new(), Box::new(root));
        a.appearance_reconcile = true;
        assert!(
            a.reconcile_appearance(),
            "first reconcile applies the intent"
        );
        assert_eq!(a.ui_scale(), 1.5);
        assert_eq!(a.text_scale(), 1.2);
        assert!(a.high_contrast());
        assert_eq!(
            a.theme.name, "High Contrast",
            "high_contrast overrides the Nord base"
        );
        // Spacing grew by the UI factor; type by UI × text.
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-1);
        assert!((a.theme.tokens.typography.scale.base - base_type * 1.5 * 1.2).abs() < 1e-1);
        assert!(
            a.layout.layout_dirty,
            "a live appearance change re-runs layout"
        );
        assert!(
            !a.reconcile_appearance(),
            "unchanged intent does not re-theme"
        );
    }
}

#[cfg(test)]
mod fullscreen_tests {
    use super::*;
    use automerge::{AutoCommit, ObjId};
    use vello::kurbo::Rect;

    use std::cell::RefCell;
    use std::rc::Rc;

    /// Lets the App's fullscreen reconcile wiring be exercised without a
    /// live window.
    struct FsRoot {
        want: Option<bool>,
        rect: Rect,
        changes: Rc<RefCell<Vec<bool>>>,
    }

    impl FsRoot {
        fn new(want: Option<bool>) -> (Self, Rc<RefCell<Vec<bool>>>) {
            let changes = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    want,
                    rect: Rect::ZERO,
                    changes: changes.clone(),
                },
                changes,
            )
        }
    }

    impl crate::widget::FullscreenProvider for FsRoot {
        fn fullscreen_intent(&self) -> Option<bool> {
            self.want
        }

        fn on_fullscreen_changed(&mut self, active: bool) {
            self.changes.borrow_mut().push(active);
        }
    }

    impl Widget for FsRoot {
        fn fullscreen_provider(&self) -> Option<&dyn crate::widget::FullscreenProvider> {
            Some(self)
        }

        fn fullscreen_provider_mut(
            &mut self,
        ) -> Option<&mut dyn crate::widget::FullscreenProvider> {
            Some(self)
        }
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _theme: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _r: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _n: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "FsRoot"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    #[test]
    fn decision_truth_table() {
        assert_eq!(fullscreen_request_decision(false, None), None);
        assert_eq!(fullscreen_request_decision(true, None), None);
        assert_eq!(fullscreen_request_decision(false, Some(false)), None);
        assert_eq!(fullscreen_request_decision(true, Some(true)), None);
        assert_eq!(fullscreen_request_decision(false, Some(true)), Some(true));
        assert_eq!(fullscreen_request_decision(true, Some(false)), Some(false));
    }

    #[test]
    fn actual_state_transitions_notify_root_once_per_edge() {
        let (root, changes) = FsRoot::new(None);
        let mut app = App::new(State::new(), Box::new(root));

        assert!(app.note_actual_fullscreen(true));
        assert!(!app.note_actual_fullscreen(true));
        assert!(app.note_actual_fullscreen(false));

        assert_eq!(*changes.borrow(), vec![true, false]);
        assert!(!app.window.fullscreen_active);
    }

    #[test]
    fn entering_fullscreen_marks_resync_and_relayout() {
        let (root, _changes) = FsRoot::new(Some(true));
        let mut app = App::new(State::new(), Box::new(root));
        app.needs_sync = false;
        app.layout.layout_dirty = false;
        assert!(app.note_actual_fullscreen(true));
        assert!(app.needs_sync);
        assert!(app.layout.layout_dirty);
    }
}

#[cfg(test)]
mod action_palette_tests {
    use super::*;
    use crate::widget::{Button, Widget};
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use tilo_actions::{Action, ActionError, ActionSpec, Doc, Resource};
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Flip {
        key: String,
    }
    const FLIP_TYPE: &str = "test.flip";
    impl Action for Flip {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            doc.put(&ROOT, self.key.as_str(), ScalarValue::Boolean(true))?;
            Ok(())
        }
    }

    /// Stands in for an app's contacts/files provider.
    struct PeopleProvider;
    impl crate::search::SearchProvider for PeopleProvider {
        fn name(&self) -> &'static str {
            "People"
        }
        fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
            ["Ada Lovelace", "Grace Hopper"]
                .into_iter()
                .filter(|n| crate::search::fuzzy_score(query, n).is_some())
                .map(|n| crate::search::SearchResult::new("People", n, format!("person:{n}")))
                .collect()
        }
    }

    struct LazyProvider;
    impl crate::search::SearchProvider for LazyProvider {
        fn name(&self) -> &'static str {
            "Lazy"
        }

        fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
            if query.eq_ignore_ascii_case("restricted") {
                vec![crate::search::SearchResult::for_action(
                    "Lazy",
                    "Restricted lazy action",
                    tilo_actions::ActionAddress::new("test.lazy:1", "flip"),
                )]
            } else {
                Vec::new()
            }
        }

        fn resolve_action_resource(
            &self,
            _address: &tilo_actions::ActionAddress,
        ) -> Option<Resource> {
            panic!("search providers must never be asked for authority")
        }
    }

    /// One open resource exporting a [`Flip`] action, plus a searchable
    /// People provider.
    struct Publisher;
    struct ConsumingKeyboardRoot;
    impl Widget for ConsumingKeyboardRoot {
        fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {}
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree.new_leaf(taffy::Style::default()).unwrap();
            nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            doc: &mut AutoCommit,
            _: &ObjId,
            handled: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
            if matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            ) {
                doc.put(&ROOT, "escaped_keyboard", true).unwrap();
                *handled = true;
            }
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn name(&self) -> &'static str {
            "consuming-keyboard-root"
        }
    }
    impl Widget for Publisher {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _p: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "publisher"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
            vec![
                Resource::new("test.obj:1", "test.obj", "Widget Object").with_action(
                    ActionSpec::immediate(
                        "flip",
                        "Flip it",
                        FLIP_TYPE,
                        &Flip {
                            key: "flipped".into(),
                        },
                    )
                    .unwrap(),
                ),
            ]
        }
        fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<Resource> {
            (id == "test.lazy:1").then(|| {
                Resource::new("test.lazy:1", "test.lazy", "Lazy object").with_action(
                    ActionSpec::immediate(
                        "flip",
                        "Flip lazy object",
                        FLIP_TYPE,
                        &Flip {
                            key: "lazy-flipped".into(),
                        },
                    )
                    .unwrap()
                    .requiring(tilo_actions::PermissionRequirement::Write),
                )
            })
        }
        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            vec![Box::new(PeopleProvider), Box::new(LazyProvider)]
        }
        fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, _p: &ObjId) {
            doc.put(&ROOT, "navigated", ScalarValue::Str(action_id.into()))
                .unwrap();
        }
    }

    struct CountingPublisher(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl Widget for CountingPublisher {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut AutoCommit,
            _p: &ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _ni: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "counting-publisher"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            vec![Resource::new("test.cache:1", "test.cache", "Cached")]
        }
    }

    #[test]
    fn resource_tree_is_materialized_once_per_exact_generation() {
        use std::sync::atomic::Ordering;

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CountingPublisher(std::sync::Arc::clone(&calls))),
        );
        let _ = app.collect_resources();
        let _ = app.collect_resources();
        app.refresh_control_action_availability();
        app.refresh_control_action_availability();
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        app.state_mut_for_test()
            .doc
            .put(&ROOT, "cache-version", 1_i64)
            .expect("test document mutation");
        let _ = app.collect_resources();
        assert_eq!(calls.load(Ordering::Relaxed), 2);

        app.invalidate_widget_resource_cache();
        let _ = app.collect_resources();
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn exhausted_resource_generation_evicts_instead_of_reusing_a_snapshot() {
        use std::sync::atomic::Ordering;

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CountingPublisher(std::sync::Arc::clone(&calls))),
        );
        app.actions.resource_generation.set(u64::MAX);

        let _ = app.collect_resources();
        app.invalidate_widget_resource_cache();
        let _ = app.collect_resources();

        assert_eq!(app.actions.resource_generation.get(), u64::MAX);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "each invalidation at the terminal generation rematerializes"
        );
    }

    #[test]
    fn exhausted_permission_generation_evicts_the_old_authority_projection() {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        assert_eq!(app.collect_local_resources().len(), 1);
        app.actions.permission_generation.set(u64::MAX);

        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));

        assert_eq!(app.actions.permission_generation.get(), u64::MAX);
        assert!(
            app.collect_local_resources().is_empty(),
            "the cached write action cannot survive a permission change"
        );
    }

    pub(super) fn synth_enter() -> WindowEvent {
        let event = KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                ),
                state: ElementState::Pressed,
            },
            Key::Named(NamedKey::Enter),
            None,
            KeyLocation::Standard,
            false,
            None,
            Key::Named(NamedKey::Enter),
        );
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    #[test]
    fn tab_focused_host_buttons_keep_enter_and_space_out_of_consuming_app() {
        for open_search in [false, true] {
            for key in [NamedKey::Enter, NamedKey::Space] {
                let mut app =
                    App::new_with_controlled_inputs(State::new(), Box::new(ConsumingKeyboardRoot));
                app.enable_document_history_chrome();
                app.init_layout();
                let layout = app.history_layout.unwrap();
                let target = if open_search {
                    layout.search.unwrap()
                } else {
                    layout.toolbar
                };
                app.set_focused_node(None);
                let mut tab = synth_enter();
                if let WindowEvent::KeyboardInput { event, .. } = &mut tab {
                    event.logical_key = Key::Named(NamedKey::Tab);
                }
                // Exercise the real native Tab event route, rather than assigning
                // the target directly or activating the Button as an AT action.
                for _ in 0..app.focusable_order.len() {
                    assert!(app.handle_tab_navigation(&tab, None));
                    if app.focused_node == Some(target) {
                        break;
                    }
                }
                assert_eq!(app.focused_node, Some(target), "Tab reaches host control");
                let ime = WindowEvent::Ime(winit::event::Ime::Commit("composed text".into()));
                let mut handled = false;
                let mut focus = None;
                assert!(app.presentation_handle_event(&ime, 1.0, &mut handled, &mut focus));
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none(),
                    "IME input focused on host chrome must never reach app content"
                );
                let mut event = synth_enter();
                if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                    event.logical_key = Key::Named(key);
                }
                let mut handled = false;
                let mut focus = None;
                assert!(app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus));
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none(),
                    "focused chrome input must never reach consuming app content"
                );
                assert!(!handled, "Button leaves activation to the native fallback");
                assert!(app.activate_focused_control_key(&event));
                assert_eq!(app.dispatch_control_invocations(), 1);
                assert_eq!(app.action_palette().is_some(), open_search);
                assert_eq!(app.document_history.is_open(), !open_search);
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none()
                );
                // Positive control: the same fixture consumes and mutates when
                // its own app node receives this event with chrome unfocused.
                app.set_focused_node(Some(layout.app));
                let mut handled = false;
                app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus);
                assert!(handled);
                assert_eq!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .and_then(|(value, _)| value.to_bool()),
                    Some(true)
                );
                app.state.doc.delete(&ROOT, "escaped_keyboard").unwrap();
                let mut handled = false;
                app.presentation_handle_event(&ime, 1.0, &mut handled, &mut focus);
                assert!(
                    handled,
                    "app-focused IME reaches the same consuming fixture"
                );
                assert_eq!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .and_then(|(value, _)| value.to_bool()),
                    Some(true)
                );
            }
        }
    }

    #[test]
    fn visible_search_uses_existing_palette_and_fresh_authority() {
        for revoke in [false, true] {
            let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
            app.action_registry_mut().register::<Flip>(FLIP_TYPE);
            app.enable_document_history_chrome();
            app.init_layout();
            let node = app
                .history_layout
                .unwrap()
                .search
                .expect("retained Search button");
            assert!(app.is_history_node(node), "Search is admitted local chrome");
            let button = app
                .document_history
                .search_mut()
                .expect("palette-capable host shows Search");
            assert_eq!(button.test_id(), Some("standalone-search-open"));
            assert!(button.perform_action(
                &crate::a11y::Action::Activate,
                &mut app.state.doc,
                &app.state.root
            ));
            assert_eq!(app.dispatch_history_controls(), 1);
            assert!(
                app.action_palette().is_some(),
                "real semantic button opens existing palette"
            );
            app.actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("Flip it");
            assert!(
                app.action_palette()
                    .unwrap()
                    .result_labels()
                    .iter()
                    .any(|label| label.contains("Flip it"))
            );
            if revoke {
                app.set_permission_ctx(PermissionContext::from_capabilities(
                    collab_types::caps::Actor::person(None, "restricted"),
                    Vec::new(),
                ));
            }
            assert!(app.action_palette_handle_key(&synth_enter()));
            assert_eq!(
                app.state.doc.get(&ROOT, "flipped").unwrap().is_some(),
                !revoke,
                "activation re-resolves current permission rather than cached row authority"
            );
            assert!(
                !app.collect_resources()
                    .iter()
                    .any(|resource| resource.id.as_str()
                        == crate::standalone_history_host::SEARCH_RESOURCE),
                "host Search never becomes an application/broker resource"
            );
            let address = tilo_actions::ActionAddress::new(
                crate::standalone_history_host::SEARCH_RESOURCE,
                "open",
            );
            assert!(
                app.invoke_resource_action(
                    &address,
                    None,
                    "same-local-name",
                    false,
                    true,
                    None,
                    None,
                    None
                )
                .is_err(),
                "broker admission and matching actor names do not grant host UI locality"
            );
        }
    }

    #[test]
    fn ctrl_k_scopes_to_resources_then_dispatch_mutates_doc_and_records_undo() {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);

        let res = app.collect_resources();
        assert_eq!(res.len(), 1, "one open resource");
        assert_eq!(res[0].kind, "test.obj");

        // Scopes the palette to those resources AND folds in the widget's
        // searchable content — one surface, both kinds.
        app.open_action_palette();
        let ap = app.action_palette().expect("palette is open");
        assert!(ap.is_open());
        let labels = ap.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Flip it")),
            "palette lists the widget's action: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "Ada Lovelace"),
            "palette folds in the widget's searchable content: {labels:?}"
        );

        assert!(app.state.doc.get(&ROOT, "flipped").unwrap().is_none());
        assert!(app.state.peek_last_undo().is_none());
        app.needs_sync = false;
        app.layout.layout_dirty = false;

        // Enter selects the top row (the action) and dispatches it.
        let consumed = app.action_palette_handle_key(&synth_enter());
        assert!(consumed, "the open palette consumed the Enter key");

        let flipped = app
            .state
            .doc
            .get(&ROOT, "flipped")
            .unwrap()
            .and_then(|(v, _)| v.to_bool());
        assert_eq!(
            flipped,
            Some(true),
            "dispatched action set the flag in the CRDT"
        );
        assert!(
            app.state.peek_last_undo().is_some(),
            "the dispatched action is undoable"
        );
        assert!(app.needs_sync, "dispatch marks the app for re-sync");
        assert!(
            app.layout.layout_dirty,
            "dispatch marks the app for relayout"
        );
        assert!(
            app.action_palette().is_none(),
            "palette closed after selection"
        );
    }

    #[test]
    fn reopening_palette_reuses_live_resource_index_until_state_mutates() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);

        app.open_action_palette();
        let first = app
            .actions
            .open_resource_search_cache
            .borrow()
            .as_ref()
            .expect("first open builds the live-resource index")
            .provider
            .clone();
        app.close_action_palette();
        app.open_action_palette();
        let second = app
            .actions
            .open_resource_search_cache
            .borrow()
            .as_ref()
            .expect("second open retains the index")
            .provider
            .clone();
        assert!(first.shares_storage_with(&second));

        app.invalidate_widget_resource_cache();
        assert!(app.actions.open_resource_search_cache.borrow().is_none());
    }

    #[test]
    fn synthesized_semantic_action_routes_back_to_its_visible_control() {
        let mut app = App::new(
            State::new(),
            Box::new(Button::new("complete", "Complete", Some("completed"))),
        );
        app.init_layout();
        let resource = app
            .collect_resources()
            .into_iter()
            .find(|resource| {
                resource.kind == "tilo.ui.control"
                    && resource.label == "Complete"
                    && resource
                        .actions
                        .iter()
                        .any(|action| action.id == "activate")
            })
            .expect("collaborative button publishes a semantic action");
        let address = tilo_actions::ActionAddress::new(resource.id.as_str(), "activate");
        let route = app
            .preferred_action_ui_route(&address)
            .expect("semantic action resolves to visible UI");

        assert_eq!(route.control_id, "complete");
        assert!(route.bounds.is_some());
        assert!(!route.disabled);
    }

    #[test]
    fn exact_widget_palette_request_resolves_only_declared_actions() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        assert!(
            app.open_action_palette_at(&tilo_actions::ActionAddress::new("test.obj:1", "flip"))
        );
        assert!(
            !app.open_action_palette_at(&tilo_actions::ActionAddress::new(
                "test.obj:1",
                "not-declared"
            )),
            "a stale or invented widget request is refused"
        );
        assert!(
            app.action_palette().is_none(),
            "a refused exact request does not leave an unrelated palette open"
        );
    }

    /// A content row does NOT dispatch — it hands its id back through
    /// [`Widget::activate_search_result`] instead.
    #[test]
    fn ctrl_k_content_row_navigates_through_the_app() {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("ada");
        let labels = app.action_palette().unwrap().result_labels();
        assert!(
            labels.iter().any(|l| l == "Ada Lovelace"),
            "content hit present: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.contains("Flip it")),
            "the action row is filtered out by the query: {labels:?}"
        );

        assert!(app.state.doc.get(&ROOT, "navigated").unwrap().is_none());
        app.needs_sync = false;
        app.layout.layout_dirty = false;

        let consumed = app.action_palette_handle_key(&synth_enter());
        assert!(consumed, "the open palette consumed the Enter key");

        let nav = app
            .state
            .doc
            .get(&ROOT, "navigated")
            .unwrap()
            .and_then(|(v, _)| v.to_str().map(str::to_string));
        assert_eq!(
            nav,
            Some("person:Ada Lovelace".to_string()),
            "the content row's id reached the app to navigate"
        );
        assert!(
            app.action_palette().is_none(),
            "navigation closes the palette"
        );
        assert!(
            app.needs_sync && app.layout.layout_dirty,
            "navigation marks a redraw"
        );
    }

    #[test]
    fn denied_lazy_search_action_never_crosses_the_host_permission_door() {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("restricted");
        // A query advances one provider page immediately; remaining providers
        // (including the host's open resources) advance on later frames.
        // Bound the drain so a pagination regression still fails this test.
        let mut progress = app
            .actions
            .action_palette
            .as_mut()
            .unwrap()
            .advance_search_page();
        for _ in 0..8 {
            if !progress.pending {
                break;
            }
            progress = app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
        }
        assert!(!progress.pending, "the tiny provider set must be exhausted");
        assert_eq!(
            app.action_palette().unwrap().result_labels(),
            vec!["Restricted lazy action"]
        );

        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(
            app.state.doc.get(&ROOT, "lazy-flipped").unwrap().is_none(),
            "the provider hit cannot bypass the host's current permission context"
        );
        assert!(
            app.action_palette().is_some(),
            "a refused row leaves search open instead of pretending it ran"
        );
    }

    #[test]
    fn projected_view_preserves_pending_edit_and_eventual_attribution() {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(Button::new("focus-me", "Focus me", None)),
        );
        app.init_layout();
        app.set_focused_node(None);
        assert!(!app.root_widget.is_focused());
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("focus");
        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(app.root_widget.is_focused());
        assert_eq!(app.state.doc.pending_ops(), pending);
        assert_eq!(app.state.doc.base_heads(), before);
        assert!(app.state.peek_last_undo().is_none());

        app.state.doc.put(ROOT, "draft", "finished").unwrap();
        let context = tilo_actions::ActionContext::for_resource(
            "Finish draft",
            "test.draft",
            "note",
            "Draft",
        );
        assert!(app.state.commit_action_authored_with_context(
            None,
            None,
            Some(app.collaboration.local_author.clone()),
            Some(context),
        ));
        let changes = app.state.doc.get_changes(&before);
        assert_eq!(changes.len(), 1, "view must not split the pending edit");
        let context = changes[0]
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .unwrap();
        assert_eq!(context.action_label, "Finish draft");
    }

    #[test]
    fn ctrl_k_invokes_a_projected_semantic_view_control() {
        let button = Button::new("drive-me", "Drive me", None);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(button));
        app.init_layout();
        app.set_focused_node(None);
        assert!(!app.root_widget.is_focused());
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("focus");

        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(
            app.root_widget.is_focused(),
            "Ctrl+K reaches the real retained control"
        );
    }
}

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
                self.inspect_node(id, |widget| widget.get_state())
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
mod view_presence_tests {
    use super::*;
    use crate::PresenceCtx;
    use crate::TextRenderer;
    use crate::Theme;
    use crate::record::{RecordCheckout, RecordId, RecordKind};
    use crate::state::State;
    use crate::view::{FreezeThaw, ViewContribution, ViewSubset};
    use crate::widget::Widget;
    use automerge::{AutoCommit, ObjId};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use taffy::TaffyTree;
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Rect as KurboRect};
    use winit::event::WindowEvent;

    /// No focus field — [`ViewSubset`] excludes the caret.
    struct ViewRoot {
        scroll: Vec<f64>,
        mode: Option<String>,
        resource: Option<String>,
        rect: KurboRect,
    }

    impl ViewRoot {
        fn new() -> Self {
            Self {
                scroll: Vec::new(),
                mode: None,
                resource: None,
                rect: KurboRect::ZERO,
            }
        }
    }

    impl Widget for ViewRoot {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: taffy::NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event_ctx(
            &mut self,
            _event: &WindowEvent,
            _ctx: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn name(&self) -> &'static str {
            "view-root"
        }
        fn get_rect(&self) -> KurboRect {
            self.rect
        }
        fn view_state(&self) -> Option<ViewContribution> {
            Some(ViewContribution {
                scroll: self.scroll.clone(),
                view_mode: self.mode.clone(),
            })
        }
        fn restore_view_state(&mut self, view: &ViewContribution) {
            self.scroll = view.scroll.clone();
            self.mode = view.view_mode.clone();
        }
        fn record_intent(&self) -> Option<RecordCheckout> {
            let r = self.resource.as_ref()?;
            RecordId::parse(r).map(|id| RecordCheckout::new(id, RecordKind::new("document")))
        }
    }

    fn app_with_root(root: ViewRoot) -> App {
        let mut app = App::new(State::new(), Box::new(root));
        app.init_layout();
        app
    }

    #[test]
    fn application_launch_receipts_are_terminal_once_and_late_replies_are_inert() {
        let mut app = app_with_root(ViewRoot::new());
        for (request_id, update) in [
            (
                1,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: tilo_actions::InvocationId::new(),
                    output: tilo_actions::ActionOutput::default(),
                },
            ),
            (
                2,
                tilo_actions::InvocationUpdate::Failed {
                    invocation: tilo_actions::InvocationId::new(),
                    error: tilo_actions::ActionFailure {
                        code: "missing_hash".into(),
                        message: "launch content is unavailable on this device".into(),
                        details: None,
                        retryable: false,
                    },
                },
            ),
            (
                3,
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation: tilo_actions::InvocationId::new(),
                    message: Some("launcher refused".into()),
                },
            ),
        ] {
            let invocation = tilo_actions::InvocationId::new();
            app.actions
                .pending_application_launches
                .insert(request_id, invocation);
            app.actions.directed_invocations.insert(
                invocation,
                DirectedInvocation {
                    wire_id: request_id + 100,
                    cancellation_requested: false,
                },
            );
            app.apply_collab_event_for_test(CollabEvent::PublishedInvocationResult {
                request_id,
                result: Ok((request_id + 10, update)),
            });
            assert!(
                !app.actions
                    .pending_application_launches
                    .contains_key(&request_id)
            );
            assert!(!app.actions.directed_invocations.contains_key(&invocation));
            // A duplicate broker receipt cannot emit a second terminal update.
            app.apply_collab_event_for_test(CollabEvent::PublishedInvocationResult {
                request_id,
                result: Err("late duplicate".into()),
            });
            assert!(!app.actions.directed_invocations.contains_key(&invocation));
        }
    }

    #[test]
    fn disconnected_application_launches_are_bounded_and_failed_once() {
        let mut app = app_with_root(ViewRoot::new());
        let invocation = tilo_actions::InvocationId::new();
        app.actions
            .pending_application_launches
            .insert(1, invocation);
        app.actions.directed_invocations.insert(
            invocation,
            DirectedInvocation {
                wire_id: 9,
                cancellation_requested: false,
            },
        );
        assert!(app.poll_directed_invocations());
        assert!(app.actions.pending_application_launches.is_empty());
        assert!(!app.actions.directed_invocations.contains_key(&invocation));
    }

    #[test]
    fn application_launch_admission_refuses_a_full_receipt_queue() {
        let mut app = app_with_root(ViewRoot::new());
        for request_id in 0..MAX_PENDING_APPLICATION_LAUNCHES as u64 {
            app.actions
                .pending_application_launches
                .insert(request_id, tilo_actions::InvocationId::new());
        }

        assert!(matches!(
            app.admit_application_launch(
                MAX_PENDING_APPLICATION_LAUNCHES as u64,
                tilo_actions::InvocationId::new(),
            ),
            Err(tilo_actions::DoorError::Unavailable { code, .. })
                if code == "application_launch_queue_full"
        ));
        assert_eq!(
            app.actions.pending_application_launches.len(),
            MAX_PENDING_APPLICATION_LAUNCHES
        );
    }

    #[test]
    fn generic_continuation_restores_view_for_apps_without_custom_metadata() {
        let mut sender = app_with_root(ViewRoot {
            scroll: vec![48.0, 12.0],
            mode: Some("grid".into()),
            resource: Some("record:stable-photo".into()),
            rect: KurboRect::ZERO,
        });
        sender.layout.last_frame_size = Some(winit::dpi::PhysicalSize::new(900, 600));
        let view = <App as FreezeThaw>::capture_view(&sender);
        let metadata = crate::collaboration::ContinuationMetadata {
            schema_version: GENERIC_VIEW_CONTINUATION_SCHEMA,
            bytes: postcard::to_stdvec(&PortableContinuationView::capture(view)).unwrap(),
        };

        // Model launcher delivery before first layout, which is the real
        // continuation order. The view waits until focus/layout nodes exist.
        let mut receiver = App::new(State::new(), Box::new(ViewRoot::new()));
        receiver.continuation.pending_restore = Some(metadata);
        receiver.apply_pending_continuation_restore();
        assert!(receiver.continuation.pending_view.is_some());
        receiver.init_layout();

        let restored = <App as FreezeThaw>::capture_view(&receiver);
        assert_eq!(restored.scroll, vec![48.0, 12.0]);
        assert_eq!(restored.view_mode.as_deref(), Some("grid"));
        assert!(receiver.continuation.pending_view.is_none());
    }

    #[test]
    fn generic_continuation_never_serializes_path_or_widget_ephemeral_state() {
        let portable = PortableContinuationView::capture(crate::view::View {
            scroll: vec![1.0],
            focus: Some(crate::view::FocusView {
                ordinal: 2,
                ephemeral: "/home/alice/private-token".into(),
            }),
            open_resource: Some("file:/home/alice/private.txt".into()),
            viewport: crate::view::ViewRect::default(),
            view_mode: Some("grid".into()),
        });
        let bytes = postcard::to_stdvec(&portable).unwrap();
        assert!(!bytes.windows(5).any(|window| window == b"/home"));
        let restored = portable.into_view();
        assert_eq!(restored.open_resource, None);
        assert_eq!(restored.focus.unwrap().ephemeral, "");
    }

    #[test]
    fn custom_semantics_cannot_replace_framework_view_fidelity() {
        let view = crate::view::View {
            scroll: vec![81.0, 13.0],
            focus: Some(crate::view::FocusView {
                ordinal: 3,
                ephemeral: "private-editor-state".into(),
            }),
            open_resource: Some("file:/private/document".into()),
            viewport: crate::view::ViewRect {
                x: 1.0,
                y: 2.0,
                width: 1280.0,
                height: 720.0,
            },
            view_mode: Some("reader".into()),
        };
        let metadata = compose_structural_continuation_metadata(
            view,
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 7,
                bytes: b"custom-safe-projection".to_vec(),
            }),
        )
        .expect("framework envelope");

        assert_eq!(metadata.schema_version, STRUCTURAL_CONTINUATION_SCHEMA);
        let facets = structural_continuation_facets(&metadata.bytes).expect("valid facets");
        assert!(facets.presentation());
        assert!(facets.semantic());
        let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(&metadata.bytes)
            .expect("structural envelope");
        assert_eq!(envelope.presentation.scroll, vec![81.0, 13.0]);
        assert_eq!(envelope.presentation.focus_ordinal, Some(3));
        assert_eq!(envelope.presentation.view_mode.as_deref(), Some("reader"));
        assert!(!metadata.bytes.windows(7).any(|window| window == b"private"));
    }

    #[test]
    fn unsafe_custom_semantics_are_stripped_without_losing_the_view() {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![42.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("grid".into()),
            },
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: b"opaque-unknown-payload".to_vec(),
            }),
        )
        .unwrap();
        let sanitized = sanitize_structural_continuation_metadata(&metadata.bytes, |_, _| false)
            .expect("valid presentation survives");
        let facets = structural_continuation_facets(&sanitized).unwrap();
        assert!(facets.presentation());
        assert!(!facets.semantic());
        let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(&sanitized).unwrap();
        assert_eq!(envelope.presentation.scroll, vec![42.0]);
        assert_eq!(envelope.presentation.view_mode.as_deref(), Some("grid"));
        assert!(!sanitized.windows(6).any(|window| window == b"opaque"));
    }

    #[test]
    fn oversized_custom_semantics_cannot_suppress_framework_view_fidelity() {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![7.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("list".into()),
            },
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: vec![b'x'; 32 * 1024],
            }),
        )
        .expect("presentation-only fallback");
        let facets = structural_continuation_facets(&metadata.bytes).unwrap();
        assert!(facets.presentation());
        assert!(!facets.semantic());
    }

    #[test]
    fn structural_continuation_restores_framework_view_before_layout() {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![64.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("list".into()),
            },
            None,
        )
        .unwrap();
        let mut receiver = App::new(State::new(), Box::new(ViewRoot::new()));
        receiver.continuation.pending_restore = Some(metadata);
        receiver.apply_pending_continuation_restore();
        assert!(receiver.continuation.pending_view.is_some());
        receiver.init_layout();
        let restored = <App as FreezeThaw>::capture_view(&receiver);
        assert_eq!(restored.scroll, vec![64.0]);
        assert_eq!(restored.view_mode.as_deref(), Some("list"));
    }

    /// Asserted on both the frame that leaves the sender (via the broadcast
    /// sink) and the frame the receiver applies.
    #[test]
    fn view_subset_broadcasts_through_real_presence_path() {
        #[cfg(windows)]
        let file_record = r"file:C:\report.md";
        #[cfg(not(windows))]
        let file_record = "file:/report.md";
        let mut sender = app_with_root(ViewRoot {
            scroll: vec![10.0, 20.0],
            mode: Some("grid".into()),
            resource: Some(file_record.into()),
            rect: KurboRect::ZERO,
        });
        sender.layout.last_frame_size = Some(winit::dpi::PhysicalSize::new(1024, 768));

        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        sender.set_presence_broadcast_sink(move |k, p| {
            sink.lock().unwrap().push((k.clone(), p.clone()));
        });

        sender.broadcast_view_presence();

        let frames = captured.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "exactly one View frame is broadcast");
        let (key, _payload) = &frames[0];
        let sent = key
            .as_view()
            .expect("the broadcast key carries a decodable View subset");
        assert_eq!(sent.scroll, vec![10.0, 20.0], "scroll rode the frame");
        assert_eq!(
            sent.open_resource.as_deref(),
            Some(file_record),
            "the open resource rode the frame"
        );
        assert_eq!(
            sent.view_mode.as_deref(),
            Some("grid"),
            "view mode rode the frame"
        );
        assert_eq!(sent.viewport.width, 1024.0, "viewport width rode the frame");
        assert_eq!(
            sent.viewport.height, 768.0,
            "viewport height rode the frame"
        );

        let mut receiver = app_with_root(ViewRoot::new());
        assert!(
            receiver
                .presence_ctx()
                .peer_view(&crate::permissions::UserId("alice".into()))
                .is_none(),
            "no View known for the peer before their broadcast arrives"
        );
        receiver.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: key.clone(),
            payload: PresencePayload::Arrived,
        });

        let peer_view = receiver
            .presence_ctx()
            .peer_view(&crate::permissions::UserId("alice".into()))
            .expect("the peer's View is stored after their broadcast");
        assert_eq!(
            peer_view, &sent,
            "the receiver stores the peer's exact View subset"
        );

        // The inert carrier payload must not be processed as a real Arrived.
        assert!(
            receiver.presence_ctx().remote_cursors.is_empty(),
            "a View frame must not register a phantom cursor"
        );
    }

    #[test]
    fn follow_binds_local_viewport_and_unfollow_releases() {
        fn deliver(app: &mut App, peer: &str, scroll: Vec<f64>, mode: &str) {
            let subset = ViewSubset {
                scroll,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some(mode.into()),
            };
            app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
                user_id: peer.into(),
                key: PresenceKey::view(&subset),
                payload: PresencePayload::Arrived,
            });
        }

        let mut app = app_with_root(ViewRoot::new());

        deliver(&mut app, "bob", vec![100.0], "list");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            Vec::<f64>::new(),
            "receiving a peer's View must not move the local view on its own"
        );

        app.follow("bob");
        let v = <App as FreezeThaw>::capture_view(&app);
        assert_eq!(
            v.scroll,
            vec![100.0],
            "follow moved the local view to the peer's scroll"
        );
        assert_eq!(
            v.view_mode.as_deref(),
            Some("list"),
            "follow adopted the peer's view mode"
        );

        deliver(&mut app, "bob", vec![250.0], "board");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            vec![250.0],
            "a followed peer's later View keeps moving the local view"
        );

        app.unfollow();
        deliver(&mut app, "bob", vec![999.0], "grid");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            vec![250.0],
            "after unfollow the local view is released and stops tracking"
        );
    }

    #[test]
    fn following_absent_peer_is_a_noop() {
        let mut app = app_with_root(ViewRoot {
            scroll: vec![7.0],
            mode: Some("grid".into()),
            resource: None,
            rect: KurboRect::ZERO,
        });

        app.follow("ghost");

        let v = <App as FreezeThaw>::capture_view(&app);
        assert_eq!(
            v.scroll,
            vec![7.0],
            "following an absent peer leaves the local view untouched"
        );
        assert_eq!(
            app.followed_peer(),
            Some("ghost"),
            "the follow intent is still latched"
        );
    }
}

#[cfg(test)]
mod semantic_tree_app_tests {
    use super::*;
    use crate::a11y_tree::{SemanticActionKind, SemanticId};
    use crate::state::State;
    use crate::widget::{Button, Widget};
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use tilo_actions::{
        Action as QAction, ActionError, ActionSpec, Doc, EncodedAction, ExecutionLocus,
        OperationKind, PermissionRequirement, Resource,
    };

    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum CounterAction {
        Bump,
        HostBump,
        SetLabel(String),
    }
    const COUNTER_TYPE: &str = "test.counter";

    #[derive(Clone)]
    struct LargeDirectoryProvider {
        catalog: crate::search::SearchCatalog,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        largest_budget: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl LargeDirectoryProvider {
        fn new(records: usize) -> Self {
            let catalog = crate::search::SearchCatalog::new("Large directory");
            for index in 0..records {
                let resource = format!("test.item:{index}");
                let address = tilo_actions::ActionAddress::new(resource.clone(), "open");
                catalog
                    .upsert(
                        resource,
                        format!("record {index} needle-{index}"),
                        crate::search::SearchResult::for_action(
                            "Large directory",
                            index.to_string(),
                            address,
                        ),
                    )
                    .expect("canonical synthetic directory row");
            }
            Self {
                catalog,
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                largest_budget: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn resource_for(address: &tilo_actions::ActionAddress) -> Option<Resource> {
            address
                .resource
                .strip_prefix("test.item:")?
                .parse::<usize>()
                .ok()?;
            Some(
                Resource::new(address.resource.clone(), "test.item", "Synthetic record")
                    .with_action(
                        ActionSpec::operation(
                            "open",
                            "Open",
                            OperationKind::View,
                            COUNTER_TYPE,
                            &CounterAction::Bump,
                        )
                        .ok()?
                        .with_contract(tilo_actions::ActionContract::view()),
                    ),
            )
        }
    }

    impl crate::search::SearchProvider for LargeDirectoryProvider {
        fn name(&self) -> &'static str {
            "Large directory"
        }

        fn action_directory_generation(&self) -> Option<u64> {
            Some(self.catalog.stats().generation)
        }

        fn action_directory_cursor_is_current(&self, cursor: &crate::search::SearchCursor) -> bool {
            self.catalog.cursor_is_current(cursor)
        }

        fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
            self.catalog.search(query)
        }

        fn search_page(
            &self,
            query: &str,
            cursor: Option<&crate::search::SearchCursor>,
            work_budget: usize,
            result_limit: usize,
        ) -> crate::search::SearchPage {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.largest_budget
                .fetch_max(work_budget, std::sync::atomic::Ordering::Relaxed);
            self.catalog
                .query_page(query, cursor, work_budget, result_limit)
        }

        fn resolve_action_resource(
            &self,
            address: &tilo_actions::ActionAddress,
        ) -> Option<Resource> {
            Self::resource_for(address)
        }
    }

    impl QAction for CounterAction {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            match self {
                CounterAction::Bump | CounterAction::HostBump => {
                    let n = match doc.get(&ROOT, "n") {
                        Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
                        _ => 0,
                    };
                    doc.put(&ROOT, "n", ScalarValue::Int(n.saturating_add(1)))?;
                }
                CounterAction::SetLabel(s) => {
                    doc.put(&ROOT, "label", s.as_str())?;
                }
            }
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            if matches!(self, CounterAction::HostBump) {
                ExecutionLocus::ServerPreferred
            } else {
                ExecutionLocus::ClientLocal
            }
        }
    }

    /// One open resource exporting an immediate `bump` and a prompted
    /// `set-label`.
    struct CounterCard {
        button: Button,
        destructive: bool,
        lazy_resource: Option<Resource>,
        directory_provider: Option<LargeDirectoryProvider>,
        directory_roots: usize,
    }

    fn counter_resource(destructive: bool) -> Resource {
        let bump = ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
            .unwrap()
            .requiring(PermissionRequirement::Write);
        let bump = if destructive {
            bump.destructive()
        } else {
            bump
        };
        Resource::new("test.counter", "test.counter", "Counter")
            .with_action(bump)
            .with_action(
                ActionSpec::immediate(
                    "host-bump",
                    "Host bump",
                    COUNTER_TYPE,
                    &CounterAction::HostBump,
                )
                .unwrap()
                .requiring(PermissionRequirement::Write),
            )
            .with_action(
                ActionSpec::operation(
                    "inspect",
                    "Inspect counter",
                    OperationKind::View,
                    COUNTER_TYPE,
                    &CounterAction::Bump,
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            )
            .with_action(
                ActionSpec::operation(
                    "ping",
                    "Ping service",
                    OperationKind::Effect,
                    COUNTER_TYPE,
                    &CounterAction::Bump,
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::default()),
            )
            .with_action(ActionSpec::prompted(
                "set-label",
                "Set label",
                "Label",
                "name…",
                ExecutionLocus::ClientLocal,
                |s: &str| {
                    EncodedAction::encode(COUNTER_TYPE, &CounterAction::SetLabel(s.to_string()))
                        .unwrap()
                },
            ))
    }

    impl CounterCard {
        fn new() -> Self {
            Self::with_destructive(false)
        }

        fn with_destructive(destructive: bool) -> Self {
            let resource = counter_resource(destructive);
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "bump").unwrap();
            Self {
                button: Button::new("bump", "Bump", None).on_action(binding),
                destructive,
                lazy_resource: None,
                directory_provider: None,
                directory_roots: 1,
            }
        }

        fn with_lazy_resource(resource: Resource) -> Self {
            Self {
                button: Button::new("lazy-resource", "Lazy resource", None),
                destructive: false,
                lazy_resource: Some(resource),
                directory_provider: None,
                directory_roots: 1,
            }
        }

        fn with_directory_provider(provider: LargeDirectoryProvider) -> Self {
            let mut card = Self::new();
            card.directory_provider = Some(provider);
            card
        }

        fn with_directory_roots(count: usize) -> Self {
            let mut card = Self::new();
            card.directory_roots = count;
            card
        }
    }
    impl Widget for CounterCard {
        fn sync(&mut self, s: &mut automerge::AutoCommit, p: &automerge::ObjId) {
            self.button.sync(s, p);
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let kid = self.button.build_layout(taffy, theme, node_map);
            let node = taffy
                .new_with_children(taffy::Style::default(), &[kid])
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event_ctx(
            &mut self,
            _event: &WindowEvent,
            _ctx: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.button]
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![&mut self.button]
        }
        fn name(&self) -> &'static str {
            "CounterCard"
        }
        fn get_rect(&self) -> KurboRect {
            KurboRect::ZERO
        }
        fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<Resource> {
            self.lazy_resource
                .is_none()
                .then(|| counter_resource(self.destructive))
                .into_iter()
                .collect()
        }
        fn action_directory_resource_page(
            &self,
            _doc: &automerge::AutoCommit,
            cursor: &[u8],
            max_results: usize,
        ) -> Option<crate::widget::ActionDirectoryResourcePage> {
            let offset = std::str::from_utf8(cursor)
                .ok()
                .and_then(|cursor| cursor.parse::<usize>().ok())
                .unwrap_or(0);
            let end = offset.saturating_add(max_results).min(self.directory_roots);
            let resources = (offset..end)
                .map(|index| {
                    if index == 0 {
                        counter_resource(self.destructive)
                    } else {
                        Resource::new(
                            format!("test.counter:{index}"),
                            "test.counter",
                            format!("Counter {index}"),
                        )
                        .with_action(
                            ActionSpec::operation(
                                "inspect",
                                "Inspect counter",
                                OperationKind::View,
                                COUNTER_TYPE,
                                &CounterAction::Bump,
                            )
                            .unwrap()
                            .with_contract(tilo_actions::ActionContract::view()),
                        )
                    }
                })
                .collect();
            Some(crate::widget::ActionDirectoryResourcePage {
                resources,
                next_cursor: end.to_string().into_bytes(),
                exhausted: end >= self.directory_roots,
            })
        }
        fn resource(&self, _doc: &automerge::AutoCommit, id: &str) -> Option<Resource> {
            if let Some(resource) = self.lazy_resource.as_ref().filter(|resource| {
                find_action_resource(std::slice::from_ref(resource), id).is_some()
            }) {
                return Some(resource.clone());
            }
            if id == "test.counter" {
                return Some(counter_resource(self.destructive));
            }
            id.strip_prefix("test.counter:")
                .and_then(|suffix| suffix.parse::<usize>().ok())
                .filter(|index| *index < self.directory_roots)
                .map(|index| {
                    Resource::new(
                        format!("test.counter:{index}"),
                        "test.counter",
                        format!("Counter {index}"),
                    )
                    .with_action(
                        ActionSpec::operation(
                            "inspect",
                            "Inspect counter",
                            OperationKind::View,
                            COUNTER_TYPE,
                            &CounterAction::Bump,
                        )
                        .unwrap()
                        .with_contract(tilo_actions::ActionContract::view()),
                    )
                })
        }
        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            self.directory_provider
                .clone()
                .map(|provider| vec![Box::new(provider) as Box<dyn crate::search::SearchProvider>])
                .unwrap_or_default()
        }
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &[COUNTER_TYPE]
        }
        fn invoke_resource_operation_v2(
            &mut self,
            kind: tilo_actions::OperationKind,
            type_id: &str,
            _payload: &[u8],
            _doc: &automerge::AutoCommit,
            _path: &automerge::ObjId,
        ) -> Result<Option<tilo_actions::ActionOutput>, String> {
            Ok(
                (kind == tilo_actions::OperationKind::Effect && type_id == COUNTER_TYPE)
                    .then(tilo_actions::ActionOutput::default),
            )
        }
    }

    fn app() -> App {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(CounterCard::new()));
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app
    }

    fn lazy_resource(id: &str) -> Resource {
        Resource::new(id, id, "Lazy counter").with_action(
            ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
                .unwrap()
                .requiring(PermissionRequirement::Write),
        )
    }

    fn lazy_app(resource: Resource) -> App {
        let mut app = App::new(
            State::new(),
            Box::new(CounterCard::with_lazy_resource(resource)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app
    }

    fn counter(app: &App) -> i64 {
        match app.state.doc.get(&ROOT, "n") {
            Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        }
    }

    fn activate_bound_button(app: &mut App) {
        let node = app.focusable_order[0];
        let pointer = app.layout.node_map[&node];
        assert!(unsafe {
            (*pointer).perform_action(
                &crate::a11y::Action::Activate,
                &mut app.state.doc,
                &app.state.root,
            )
        });
    }

    #[test]
    fn lazy_resource_fallback_invokes_after_retained_cache_snapshot() {
        let resource = lazy_resource("test.lazy");
        let mut app = lazy_app(resource);
        let address = tilo_actions::ActionAddress::new("test.lazy", "bump");
        let snapshot = app.collect_resources();
        assert!(
            tilo_actions::door::resolve(&snapshot, &address).is_none(),
            "the retained snapshot must omit the lazy action"
        );
        assert!(
            app.actions.resource_cache.borrow().is_some(),
            "the snapshot is retained before fallback"
        );

        let result = app.invoke_resource_action(
            &address,
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );

        assert!(
            result.is_ok(),
            "the lazy resource should resolve: {result:?}"
        );
        assert_eq!(counter(&app), 1);
    }

    #[test]
    fn lazy_resource_fallback_respects_local_permissions() {
        let mut app = lazy_app(lazy_resource("test.lazy"));
        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));

        let denied = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.lazy", "bump"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(matches!(denied, Err(tilo_actions::DoorError::Unknown(_))));
        assert_eq!(counter(&app), 0, "the denied fallback must not run");

        app.set_permission_ctx(PermissionContext::permissive());
        let allowed = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.lazy", "bump"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(
            allowed.is_ok(),
            "restoring write permission should admit it"
        );
        assert_eq!(counter(&app), 1);
    }

    #[test]
    fn recursive_lazy_resource_remains_discoverable_and_invocable() {
        let child = lazy_resource("test.lazy.child");
        let tree =
            Resource::new("test.lazy.parent", "test.lazy.parent", "Parent").with_child(child);
        let mut app = lazy_app(tree);
        let resolved = crate::widget::resolve_widget_resource(
            app.root_widget.as_ref(),
            &app.state.doc,
            "test.lazy.child",
        )
        .expect("the recursive lazy child is discoverable");
        assert_eq!(
            find_action_resource(&[resolved], "test.lazy.child")
                .map(|resource| resource.id.as_str()),
            Some("test.lazy.child")
        );

        let result = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.lazy.child", "bump"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "the recursive lazy action should resolve: {result:?}"
        );
        assert_eq!(counter(&app), 1);
    }

    #[test]
    fn action_bound_control_uses_fresh_door_and_records_history() {
        let mut app = app();
        activate_bound_button(&mut app);
        assert_eq!(counter(&app), 0, "control only queued an intent");
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(counter(&app), 1);
        assert!(app.state.peek_last_undo().is_some());
        assert_eq!(
            app.control_action_coverage()[0].status,
            ControlBindingStatus::Resolved
        );
    }

    #[test]
    fn a11y_activation_drains_an_action_bound_control_before_returning() {
        let mut app = app();
        let button = app.focusable_order[0];
        // Retained widget ids have the same stable AccessKit mapping used by
        // the native provider. This exercises the public AT-SPI action path,
        // rather than manually draining the widget's queued intent.
        let accesskit_button = accesskit::NodeId(u64::from(button) + 1);

        assert_eq!(
            app.dispatch_a11y_action(accesskit_button, &crate::a11y::Action::Activate),
            None
        );
        assert_eq!(
            counter(&app),
            1,
            "AT-SPI activation must apply the bound action before success is returned"
        );
        assert!(app.state.peek_last_undo().is_some());
    }

    #[test]
    fn directed_task_context_survives_into_action_history() {
        let mut app = app();
        let resources = app.collect_resources();
        assert!(!resources.is_empty(), "test action surface was rejected");
        let ambient = tilo_actions::ActionContext::default()
            .with_app("org.tilo.Agent", "Agent")
            .with_purpose("Make the opening concrete")
            .with_group("task:opening", "Rewrite the opening", "task");

        let result = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.counter", "bump"),
            None,
            "agent:Scribe",
            true,
            true,
            None,
            Some(tilo_actions::InvocationId::new()),
            Some(&ambient),
        );

        assert!(result.is_ok(), "the directed action failed: {result:?}");
        assert_eq!(counter(&app), 1, "the directed action was applied");
        let changes = app.state.doc.get_changes(&[]);
        let committed = changes.last().expect("the directed action committed");
        assert_eq!(committed.actor_id().to_bytes(), b"agent:Scribe");
        let context = committed
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .expect("the action commit retained semantic provenance");
        assert_eq!(context.action_label, "Bump");
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("test.counter"),
            "the concrete addressed resource stays authoritative"
        );
        assert_eq!(context.purpose, ambient.purpose);
        assert_eq!(context.group, ambient.group);
        assert_eq!(context.app, ambient.app);
        assert_eq!(
            app.state
                .peek_last_undo()
                .and_then(|entry| entry.author.as_ref()),
            Some(&crate::permissions::UserId("agent:Scribe".into())),
            "undo attribution and durable history use the same actor"
        );
    }

    #[test]
    fn undecodable_document_action_preserves_pending_edit() {
        let resource = Resource::new("test.invalid", "test.invalid", "Invalid action").with_action(
            ActionSpec::immediate(
                "decode",
                "Decode",
                "test.unregistered",
                &CounterAction::Bump,
            )
            .unwrap(),
        );
        let mut app = lazy_app(resource);
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        let result = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.invalid", "decode"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(
            matches!(result, Err(tilo_actions::DoorError::UnregisteredType(ref id))
            if id == "test.unregistered"),
            "unexpected result: {result:?}"
        );
        assert_eq!(app.state.doc.pending_ops(), pending);
        assert_eq!(app.state.doc.base_heads(), before);
        assert!(app.state.peek_last_undo().is_none());
    }

    #[test]
    fn refused_and_routed_actions_preserve_pending_edit() {
        let mut app = app();
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        for (action, actor) in [
            ("host-bump", app.collaboration.local_author.0.clone()),
            ("inspect", "agent:Scribe".to_string()),
            ("missing", app.collaboration.local_author.0.clone()),
        ] {
            let result = app.invoke_resource_action(
                &tilo_actions::ActionAddress::new("test.counter", action),
                None,
                &actor,
                false,
                false,
                None,
                None,
                None,
            );
            assert!(result.is_err(), "unadmitted action must fail: {action}");
            assert_eq!(app.state.doc.pending_ops(), pending, "{action}");
            assert_eq!(app.state.doc.base_heads(), before, "{action}");
            assert!(app.state.peek_last_undo().is_none());
        }
        app.state.doc.put(ROOT, "draft", "finished").unwrap();
        let context = tilo_actions::ActionContext::for_resource(
            "Finish draft",
            "test.draft",
            "note",
            "Draft",
        );
        assert!(app.state.commit_action_authored_with_context(
            None,
            None,
            Some(app.collaboration.local_author.clone()),
            Some(context),
        ));
        let changes = app.state.doc.get_changes(&before);
        assert_eq!(changes.len(), 1, "refusal must not split the pending edit");
        let context = changes[0]
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .unwrap();
        assert_eq!(context.action_label, "Finish draft");
    }

    #[test]
    fn host_required_document_action_runs_only_after_broker_admission() {
        let mut app = app();
        let address = tilo_actions::ActionAddress::new("test.counter", "host-bump");

        let local = app.invoke_resource_action(
            &address,
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(local.is_err(), "an unbrokered client must fail closed");
        assert_eq!(counter(&app), 0);

        let admitted = app.invoke_resource_action(
            &address,
            None,
            "agent:Scribe",
            true,
            true,
            None,
            Some(tilo_actions::InvocationId::new()),
            None,
        );
        assert!(admitted.is_ok(), "the authoritative broker admitted it");
        assert_eq!(counter(&app), 1);
        assert_eq!(
            app.state
                .peek_last_undo()
                .and_then(|entry| entry.author.as_ref()),
            Some(&crate::permissions::UserId("agent:Scribe".into()))
        );
    }

    #[test]
    fn directed_view_action_cannot_steal_the_local_view() {
        let mut app = app();
        let invocation = tilo_actions::InvocationId::new();
        let error = app
            .invoke_resource_action(
                &tilo_actions::ActionAddress::new("test.counter", "inspect"),
                None,
                "agent:Scribe",
                true,
                true,
                None,
                Some(invocation),
                None,
            )
            .expect_err("a remote presentation action must be refused");

        assert_eq!(counter(&app), 0, "the private view action never ran");
        assert!(
            matches!(
                error,
                tilo_actions::DoorError::Unavailable { ref code, .. } if code == "private_view"
            ),
            "unexpected refusal: {error:?}"
        );
    }

    #[test]
    fn local_control_result_does_not_broadcast_into_component_siblings() {
        use crate::application_test_support::ProbeHost;
        use crate::widget::{Column, TileHost};
        for closed in [false, true] {
            let owner = ProbeHost::new();
            let (component, sibling_seen) =
                crate::widget::control_result_probe::component(&owner, 11, false);
            let (legacy, legacy_seen) = crate::widget::control_result_probe::Probe::new(false);
            let root = Column::new()
                .push(Box::new(legacy))
                .push(Box::new(TileHost::from_component(component)));
            let mut host = App::new_with_controlled_inputs(State::new(), Box::new(root));
            if closed {
                host.close_components();
            }
            legacy_seen.enqueue("missing");
            host.dispatch_control_invocations();
            assert_eq!(legacy_seen.results.borrow().len(), 1);
            assert!(
                sibling_seen.results.borrow().is_empty(),
                "legacy feedback must not enter bound sibling"
            );
            assert_eq!(sibling_seen.calls.get(), 0);
        }
    }

    #[test]
    fn bound_local_control_result_targets_only_the_emitting_instance() {
        use crate::application_test_support::ProbeHost;
        use crate::widget::{Column, TileHost};
        let owner = ProbeHost::new();
        let (first, first_seen) = crate::widget::control_result_probe::component(&owner, 11, false);
        let (second, second_seen) =
            crate::widget::control_result_probe::component(&owner, 12, false);
        let root = Column::new()
            .push(Box::new(TileHost::from_component(first)))
            .push(Box::new(TileHost::from_component(second)));
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(root));
        first_seen.enqueue("run");
        host.dispatch_control_invocations();
        assert_eq!(first_seen.results.borrow().len(), 1);
        assert!(second_seen.results.borrow().is_empty());
        assert_eq!(first_seen.calls.get(), 1);
        assert_eq!(second_seen.calls.get(), 0);
    }

    #[test]
    fn bound_local_control_result_reports_permission_refusal_and_success() {
        use crate::application_test_support::{ProbeHost, runner_inputs};
        for accepted in [false, true] {
            let owner = ProbeHost::new();
            let (component, observed) =
                crate::widget::control_result_probe::component(&owner, 11, accepted);
            let binding = component.callback_binding();
            let mut host = App::from_component(component, runner_inputs()).unwrap();
            observed.enqueue("run");
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1);
            let expected = if accepted {
                crate::widget::ResourceOperationStart::Accepted
            } else {
                crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )
            };
            assert_eq!(*observed.results.borrow(), vec![Ok(expected)]);
            observed.enqueue("run");
            *binding.permissions.borrow_mut() = PermissionContext::from_capabilities(
                collab_types::caps::Actor::person(Some(collab_types::role::Role::Viewer), "reader"),
                Vec::new(),
            );
            let before = owner
                .records
                .read(&binding.checkout.id, automerge::AutoCommit::base_heads)
                .unwrap();
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1, "revoked permission blocks handler");
            assert_eq!(
                owner
                    .records
                    .read(&binding.checkout.id, automerge::AutoCommit::base_heads)
                    .unwrap(),
                before
            );
            assert_eq!(observed.results.borrow().len(), 2);
            assert!(matches!(observed.results.borrow().last(), Some(Err(_))));
        }
    }

    #[test]
    fn local_control_result_keeps_confirmation_as_refusal() {
        let (mut probe, observed) = crate::widget::control_result_probe::Probe::new(false);
        probe.confirmation = true;
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
        observed.enqueue("run");
        host.dispatch_control_invocations();
        assert_eq!(observed.calls.get(), 0);
        assert_eq!(
            *observed.results.borrow(),
            vec![Err(tilo_actions::DoorError::ConfirmationRequired)]
        );
        assert!(host.action_palette().is_some());
    }

    #[test]
    fn confirmed_control_preserves_captured_arguments_through_palette_dispatch() {
        let (mut probe, observed) = crate::widget::control_result_probe::Probe::new(false);
        probe.confirmation = true;
        probe.typed_text = true;
        observed.enqueue_text("run", "captured before confirmation");
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));

        assert_eq!(host.dispatch_control_invocations(), 1);
        assert!(
            host.action_palette()
                .is_some_and(crate::action_palette::ActionPalette::in_argument_mode),
            "captured arguments enter confirmation directly"
        );
        assert!(observed.payloads.borrow().is_empty());

        host.actions
            .action_palette
            .as_mut()
            .expect("confirmation palette is open")
            .set_query("CONFIRM");
        assert!(host.action_palette_handle_key(&action_palette_tests::synth_enter()));
        assert_eq!(
            observed.payloads.borrow().as_slice(),
            ["captured before confirmation"],
            "confirmation dispatches the original intent arguments"
        );
    }

    #[test]
    fn local_control_result_reports_completed_and_accepted() {
        use crate::widget::control_result_probe::Probe;
        for accepted in [false, true] {
            let (probe, observed) = Probe::new(accepted);
            let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
            observed.enqueue("run");
            host.dispatch_control_invocations();
            assert_eq!(
                observed.calls.get(),
                1,
                "accepted={accepted}; feedback={:?}",
                observed.results.borrow()
            );
            let expected = if accepted {
                crate::widget::ResourceOperationStart::Accepted
            } else {
                crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )
            };
            assert_eq!(*observed.results.borrow(), vec![Ok(expected)]);
            observed.enqueue("missing");
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1);
            assert!(matches!(
                observed.results.borrow().last(),
                Some(Err(tilo_actions::DoorError::Unknown(_)))
            ));
        }
    }

    #[test]
    fn local_control_result_reports_fresh_permission_refusal_without_mutation() {
        use crate::widget::control_result_probe::Probe;
        let (probe, observed) = Probe::new(false);
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
        observed.enqueue("run");
        let readonly = PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(Some(collab_types::role::Role::Viewer), "reader"),
            Vec::new(),
        );
        host.set_permission_ctx(readonly);
        let before = host.state.doc.base_heads();
        host.dispatch_control_invocations();
        assert_eq!(observed.calls.get(), 0, "permission checked before handler");
        assert_eq!(host.state.doc.base_heads(), before);
        assert_eq!(
            automerge::transaction::Transactable::pending_ops(&host.state.doc),
            0
        );
        assert!(matches!(
            observed.results.borrow().as_slice(),
            [Err(tilo_actions::DoorError::Unknown(_))]
        ));
        assert!(observed.pending.borrow().is_empty());
    }

    #[test]
    fn local_effect_completion_does_not_mutate_the_app_document() {
        let mut app = app();
        let before = app.state.doc.get_heads();
        let result = app.invoke_resource_action(
            &tilo_actions::ActionAddress::new("test.counter", "ping"),
            None,
            &app.collaboration.local_author.0.clone(),
            false,
            false,
            None,
            None,
            None,
        );
        assert!(matches!(
            result,
            Ok(crate::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.state.doc.get_heads(), before);
        assert!(app.state.peek_last_undo().is_none());
    }

    #[test]
    fn permission_revocation_and_restore_refresh_bound_control_presentation() {
        let mut app = app();
        let node = app.focusable_order[0];
        let pointer = app.layout.node_map[&node];
        assert!(unsafe { (*pointer).is_enabled() });

        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));
        assert!(!unsafe { (*pointer).is_enabled() });
        assert!(!unsafe {
            (*pointer).perform_action(
                &crate::a11y::Action::Activate,
                &mut app.state.doc,
                &app.state.root,
            )
        });

        app.set_permission_ctx(PermissionContext::permissive());
        assert!(unsafe { (*pointer).is_enabled() });
    }

    #[test]
    fn action_bound_control_never_auto_confirms_destructive_action() {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_destructive(true)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        activate_bound_button(&mut app);
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(counter(&app), 0);
        assert!(app.state.peek_last_undo().is_none());
        assert!(app.action_palette().is_some(), "confirmation UI is open");
    }

    #[test]
    fn action_bound_background_control_remains_pending_until_terminal_update() {
        let resource =
            crate::widget::resource_actions::resource("test.job", "Job", "run", "Run", true);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "run").unwrap();
        let button = Button::new("run", "Run", None).on_action(binding);
        let root = crate::widget::resource_actions::background(Box::new(button), resource, |_| {
            let (sender, receiver) = std::sync::mpsc::channel();
            sender
                .send(Ok(tilo_actions::ActionOutput::default()))
                .unwrap();
            Ok(receiver)
        });
        let mut app = App::new_with_controlled_inputs(State::new(), root);
        app.init_layout();
        activate_bound_button(&mut app);
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(
            app.actions.local_invocations.len(),
            1,
            "Accepted is not success"
        );
        assert!(app.poll_directed_invocations());
        assert!(
            app.actions.local_invocations.is_empty(),
            "terminal result closes the tracked local job"
        );
    }
    fn label(app: &App) -> String {
        match app.state.doc.get(&ROOT, "label") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    #[test]
    fn refresh_populates_the_tree_from_app_resources() {
        let mut app = app();
        app.refresh_semantic_tree();
        let tree = app.semantic_tree();
        let node = tree
            .node(&SemanticId("r:test.counter".into()))
            .expect("the app's open resource is a semantic node");
        assert_eq!(node.name.as_deref(), Some("Counter"));
        assert!(
            node.action("bump").is_some(),
            "the resource's action is an affordance"
        );
        assert!(node.action("set-label").is_some());
        // The root owns it (resources hang off the window root).
        let root = tree.node(tree.root()).unwrap();
        assert!(root.children.contains(&SemanticId("r:test.counter".into())));
    }

    #[test]
    fn identity_is_stable_across_refreshes() {
        let mut app = app();
        app.refresh_semantic_tree();
        let ids_before: Vec<SemanticId> =
            app.semantic_tree().iter().map(|n| n.id.clone()).collect();
        let delta = app.refresh_semantic_tree();
        assert!(
            delta.is_empty(),
            "a stable frame yields no delta, got {delta:?}"
        );
        let ids_after: Vec<SemanticId> = app.semantic_tree().iter().map(|n| n.id.clone()).collect();
        assert_eq!(ids_before, ids_after, "node identity survives a refresh");
    }

    #[test]
    fn semantic_action_carries_the_same_encoded_action_the_affordance_dispatches() {
        let mut app = app();
        app.refresh_semantic_tree();
        let bump = app
            .semantic_tree()
            .action(&SemanticId("r:test.counter".into()), "bump")
            .expect("bump affordance");

        let mut host = crate::action_host::ActionHost::new();
        let mut host_doc = automerge::AutoCommit::new();
        host.dispatch(&mut host_doc, COUNTER_TYPE, &CounterAction::Bump);
        let from_affordance = host
            .last_dispatched()
            .expect("the affordance dispatched")
            .clone();

        match &bump.kind {
            SemanticActionKind::App(enc) => assert_eq!(
                enc, &from_affordance,
                "the a11y node exposes the same EncodedAction the on-screen control dispatches"
            ),
            other => panic!("expected an app action, got {other:?}"),
        }
    }

    #[test]
    fn invoking_an_immediate_action_dispatches_it_through_the_action_model() {
        let mut app = app();
        app.refresh_semantic_tree();
        let id = SemanticId("r:test.counter".into());
        assert_eq!(counter(&app), 0);

        assert!(app.invoke_semantic_action(&id, "bump", None).unwrap());
        assert_eq!(
            counter(&app),
            1,
            "the invoked action mutated the shared doc"
        );
        assert!(app.invoke_semantic_action(&id, "bump", None).unwrap());
        assert_eq!(counter(&app), 2);

        // An unknown action name is a graceful no-op.
        assert!(!app.invoke_semantic_action(&id, "nope", None).unwrap());
        assert_eq!(counter(&app), 2);
    }

    #[test]
    fn invoking_a_prompted_action_uses_the_supplied_argument() {
        let mut app = app();
        app.refresh_semantic_tree();
        let id = SemanticId("r:test.counter".into());
        assert!(
            app.invoke_semantic_action(&id, "set-label", Some("hello"))
                .unwrap()
        );
        assert_eq!(
            label(&app),
            "hello",
            "the prompted argument reached the built action"
        );
    }

    #[test]
    fn invoke_matches_a_direct_affordance_dispatch_byte_for_byte() {
        let mut app = app();
        app.refresh_semantic_tree();
        app.invoke_semantic_action(&SemanticId("r:test.counter".into()), "bump", None)
            .unwrap();

        let mut host = crate::action_host::ActionHost::new();
        let mut host_doc = automerge::AutoCommit::new();
        host.dispatch(&mut host_doc, COUNTER_TYPE, &CounterAction::Bump);

        let via_a11y = counter(&app);
        let via_affordance = match host_doc.get(&ROOT, "n") {
            Ok(Some((v, _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        };
        assert_eq!(
            via_a11y, via_affordance,
            "both paths produce the same result"
        );
    }

    #[test]
    fn virtual_tree_rows_dispatch_through_platform_and_semantic_routes() {
        let root = crate::widget::TreeNode::branch(
            1,
            "Root",
            (),
            vec![crate::widget::TreeNode::leaf(2, "Child", ())],
        );
        let mut app = App::new(
            State::new(),
            Box::new(
                crate::widget::TreeView::new(root)
                    .with_id("items-tree")
                    .with_expanded([1]),
            ),
        );
        app.init_layout();

        app.refresh_semantic_tree();
        let child = app
            .semantic_tree()
            .iter()
            .find(|node| {
                node.role == crate::a11y::Role::TreeItem && node.name.as_deref() == Some("Child")
            })
            .map(|node| node.id.clone())
            .expect("semantic child row");
        assert!(
            app.invoke_semantic_action(&child, "activate", None)
                .expect("semantic row activation")
        );
        app.refresh_semantic_tree();
        assert!(
            app.semantic_tree()
                .node(&child)
                .is_some_and(|node| node.state.selected)
        );

        let update = app.build_a11y_tree_update(1.0);
        assert_eq!(
            update
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == accesskit::Role::TreeItem)
                .count(),
            2
        );
        let root_row = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .map(|(id, node)| {
                assert!(node.supports_action(accesskit::Action::Collapse));
                *id
            })
            .expect("virtual root row");
        let route = crate::accesskit_bridge::resolve_node_id(
            app.root_widget.as_ref(),
            &app.layout.node_map,
            root_row,
        )
        .expect("root row route");
        assert!(matches!(
            route,
            crate::accesskit_bridge::A11yTarget::Synthetic {
                semantic_target: Some(ref target),
                ..
            } if target == "node:1"
        ));
        assert_eq!(
            app.dispatch_a11y_action(root_row, &crate::a11y::Action::Collapse),
            None
        );
        app.refresh_semantic_tree();
        let root_semantics = app
            .semantic_tree()
            .iter()
            .find(|node| {
                node.role == crate::a11y::Role::TreeItem && node.name.as_deref() == Some("Root")
            })
            .expect("root semantics after collapse");
        assert_eq!(root_semantics.state.expanded, Some(false));

        let collapsed = app.build_a11y_tree_update(1.0);
        let rows = collapsed
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "collapsing removes the child row");
        assert!(rows[0].1.supports_action(accesskit::Action::Expand));
    }

    #[test]
    fn nav_shell_accessible_autocomplete_edits_and_selects_through_host_dispatch() {
        let search = crate::widget::Autocomplete::new()
            .with_id("city-search")
            .with_placeholder("Search city")
            .with_suggester(|query| {
                if query == "Ber" {
                    vec![crate::widget::Suggestion::new("Berlin")]
                } else {
                    Vec::new()
                }
            });
        let nav =
            crate::widget::NavShell::new(Box::new(search), Box::new(crate::widget::Column::new()));
        let mut app = App::new(State::new(), Box::new(nav));
        app.init_layout();
        let update = app.build_a11y_tree_update(1.0);
        let field = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Search city"))
            .unwrap()
            .0;
        app.dispatch_a11y_action(field, &crate::a11y::Action::SetValue("Ber".into()));
        let update = app.build_a11y_tree_update(1.0);
        let result = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Berlin"))
            .expect("native edit must publish a result")
            .0;
        app.dispatch_a11y_action(result, &crate::a11y::Action::Activate);
        let update = app.build_a11y_tree_update(1.0);
        let field = &update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Search city"))
            .unwrap()
            .1;
        assert_eq!(field.value(), Some("Berlin"));
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Berlin"))
        );
    }

    #[test]
    fn retained_tree_delegate_is_nested_once_and_keeps_its_own_action() {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        let captured = std::rc::Rc::clone(&presses);
        let root = crate::widget::TreeNode::leaf(7, "Item", ());
        let tree = crate::widget::TreeView::new(root).with_row_delegate(move |_, id, _| {
            let button_id = format!("action-{id}");
            let captured = std::rc::Rc::clone(&captured);
            Box::new(
                crate::widget::Button::new(&button_id, "Action", None).on_click(move || {
                    captured.set(captured.get().saturating_add(1));
                }),
            )
        });
        let mut app = App::new(State::new(), Box::new(tree));
        app.init_layout();

        let update = app.build_a11y_tree_update(1.0);
        let buttons = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::Button)
            .collect::<Vec<_>>();
        assert_eq!(buttons.len(), 1, "delegate is not announced twice");
        let button_id = buttons[0].0;
        let row = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .map(|(_, node)| node)
            .expect("tree item");
        assert_eq!(row.children(), &[button_id]);
        assert!(
            row.bounds().is_some(),
            "platform row keeps viewport geometry"
        );
        assert_eq!(
            app.dispatch_a11y_action(button_id, &crate::a11y::Action::Activate),
            None
        );
        assert_eq!(presses.get(), 1);

        app.refresh_semantic_tree();
        let semantic_buttons = app
            .semantic_tree()
            .iter()
            .filter(|node| node.role == crate::a11y::Role::Button)
            .collect::<Vec<_>>();
        assert_eq!(semantic_buttons.len(), 1);
        let semantic_row = app
            .semantic_tree()
            .iter()
            .find(|node| node.role == crate::a11y::Role::TreeItem)
            .expect("semantic tree item");
        assert!(semantic_row.children.contains(&semantic_buttons[0].id));
        assert!(
            semantic_row.bounds.is_some(),
            "inspection row keeps viewport geometry"
        );
        assert!(
            !semantic_row.state.selected,
            "button activation does not select row"
        );
    }

    #[test]
    fn synthetic_drawn_control_dispatches_through_retained_semantic_tree() {
        let changed = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
        let captured = std::rc::Rc::clone(&changed);
        let group = crate::widget::ToggleGroup::new(
            vec![
                crate::widget::toggle_group::Segment::new("Light"),
                crate::widget::toggle_group::Segment::new("Dark"),
            ],
            0,
        )
        .on_change(move |index| captured.set(index));
        let mut app = App::new(State::new(), Box::new(group));
        app.init_layout();
        app.refresh_semantic_tree();
        let dark = app
            .semantic_tree()
            .find_by_name("Dark")
            .map(|node| node.id.clone())
            .expect("synthetic Dark segment");

        assert!(
            app.invoke_semantic_action(&dark, "activate", None)
                .expect("semantic dispatch")
        );
        assert_eq!(changed.get(), 1);
    }

    #[test]
    fn effectful_control_is_not_exported_without_a_domain_contract() {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        let captured = std::rc::Rc::clone(&presses);
        let button = Button::new("drive-me", "Drive me", None).on_click(move || {
            captured.set(captured.get().saturating_add(1));
        });
        let mut app = App::new(State::new(), Box::new(button));
        app.init_layout();

        let resource = app
            .collect_resources()
            .into_iter()
            .find(|resource| resource.kind == "tilo.ui.control" && resource.label == "Drive me");
        assert!(resource.is_none_or(|resource| {
            resource
                .actions
                .iter()
                .all(|action| action.id != "activate")
        }));
        assert_eq!(presses.get(), 0);
    }

    fn directory_query(
        revision: impl Into<String>,
        parent_resource: impl Into<String>,
        max_results: u32,
        max_bytes: u32,
        max_work: u32,
    ) -> runtime_collab::ActionDirectoryQuery {
        runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: revision.into(),
            cursor: Vec::new(),
            needle: String::new(),
            parent_resource: parent_resource.into(),
            max_results,
            max_bytes,
            max_work,
            max_depth: 8,
        }
    }

    #[test]
    fn directory_pages_parent_actions_and_refuses_stale_resolution() {
        let mut app = app();
        let root = app.action_directory_page(directory_query("", "", 8, 8_192, 8));
        assert_eq!(root.rows.len(), 1);
        assert_eq!(root.rows[0].resource, "test.counter");
        assert!(root.rows[0].action.is_none());
        assert!(root.rows[0].has_children);

        let actions =
            app.action_directory_page(directory_query(root.revision, "test.counter", 8, 8_192, 8));
        assert!(actions.rows.len() >= 5, "parent exposes its action rows");
        assert!(actions.rows.iter().all(|row| row.action.is_some()));
        assert!(matches!(
            app.resolve_action_directory("stale", "test.counter", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
    }

    #[test]
    fn broker_request_ids_use_the_final_value_then_fail_closed() {
        let mut app = app();
        app.actions.next_broker_request_id = Some(u64::MAX);
        assert_eq!(app.take_broker_request_id().unwrap(), u64::MAX);
        assert!(matches!(
            app.take_broker_request_id(),
            Err(tilo_actions::DoorError::Unavailable { code, .. })
                if code == "broker_request_identity_exhausted"
        ));
    }

    #[test]
    fn action_directory_cursor_keys_do_not_reuse_a_process_counter() {
        let mut app = app();
        let first = app.action_directory_cursor_key();
        let second = app.action_directory_cursor_key();
        assert_ne!(first, second);
        assert!(first.starts_with("tilo-dir-"));
        assert!(second.starts_with("tilo-dir-"));
    }

    #[test]
    fn directory_hard_caps_never_materialize_an_oversized_row() {
        let mut app = app();
        let page = app.action_directory_page(directory_query("", "", 1, 1, 1));
        assert!(
            page.rows.is_empty(),
            "one byte cannot contain a resource row"
        );
        assert!(page.budget_exhausted);
        assert!(!page.exhausted);
    }

    #[test]
    fn directory_pages_the_129th_widget_root_without_hiding_it() {
        let mut app = App::new(
            State::new(),
            Box::new(CounterCard::with_directory_roots(
                ACTION_DIRECTORY_MAX_RESULTS + 1,
            )),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        let first = app.action_directory_page(directory_query(
            "",
            "",
            u32::try_from(ACTION_DIRECTORY_MAX_RESULTS).unwrap_or(u32::MAX),
            64 * 1024,
            u32::try_from(ACTION_DIRECTORY_MAX_WORK).unwrap_or(u32::MAX),
        ));
        assert_eq!(first.rows.len(), ACTION_DIRECTORY_MAX_RESULTS);
        assert!(!first.exhausted);
        let tail = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: first.revision.clone(),
            cursor: first.next_cursor,
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 8,
            max_bytes: 8_192,
            max_work: 32,
            max_depth: 8,
        });
        assert_eq!(tail.rows.len(), 1);
        assert_eq!(tail.rows[0].resource, "test.counter:128");
        assert!(tail.exhausted);
    }

    #[test]
    fn directory_finds_a_100k_tail_without_scanning_the_corpus() {
        let provider = LargeDirectoryProvider::new(100_000);
        let calls = std::sync::Arc::clone(&provider.calls);
        let largest_budget = std::sync::Arc::clone(&provider.largest_budget);
        let mut app = App::new(
            State::new(),
            Box::new(CounterCard::with_directory_provider(provider)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        let page = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: String::new(),
            cursor: Vec::new(),
            needle: "needle-99999".into(),
            parent_resource: String::new(),
            max_results: 8,
            max_bytes: 8_192,
            max_work: 8,
            max_depth: 8,
        });
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].resource, "test.item:99999");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            largest_budget.load(std::sync::atomic::Ordering::Relaxed) <= 8,
            "the adapter passed the caller's bounded work budget to the retained index"
        );
    }

    #[test]
    fn directory_cursor_saturation_evicts_old_cursors_as_stale() {
        let mut app = app();
        let revision = app.action_directory_revision();
        let first = app.action_directory_next_cursor(ActionDirectoryCursor {
            revision: revision.clone(),
            needle: String::new(),
            parent_resource: String::new(),
            provider_index: 0,
            provider_cursor: None,
            browse_offset: 0,
            root_widget_index: 0,
            root_provider_cursor: None,
        });
        for offset in 1..=ACTION_DIRECTORY_MAX_CURSORS {
            let _ = app.action_directory_next_cursor(ActionDirectoryCursor {
                revision: revision.clone(),
                needle: String::new(),
                parent_resource: String::new(),
                provider_index: 0,
                provider_cursor: None,
                browse_offset: offset,
                root_widget_index: 0,
                root_provider_cursor: None,
            });
        }
        assert!(app.actions.action_directory_cursors.len() <= ACTION_DIRECTORY_MAX_CURSORS);
        let page = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision,
            cursor: first,
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 1_024,
            max_work: 1,
            max_depth: 1,
        });
        assert!(page.stale_cursor);
    }
}

#[cfg(test)]
mod damage_lifecycle_tests {
    use super::*;
    use crate::widget::TaffyTreeExt;
    use automerge::{AutoCommit, ObjId};
    use std::collections::HashMap;

    struct DrawUpdatedDamage {
        damage: KurboRect,
        dirty: bool,
    }

    impl Widget for DrawUpdatedDamage {
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.leaf_node(taffy::Style::default());
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
            self.damage = KurboRect::new(80.0, 0.0, 100.0, 8.0);
        }

        fn handle_event(
            &mut self,
            _event: &WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<NodeId>,
            _node_id: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }

        fn name(&self) -> &'static str {
            "DrawUpdatedDamage"
        }

        fn get_rect(&self) -> KurboRect {
            self.damage
        }

        fn needs_redraw(&self) -> bool {
            self.dirty
        }

        fn collect_damage(&self, regions: &mut Vec<KurboRect>) {
            regions.push(self.damage);
        }

        fn mark_drawn(&mut self) {
            self.dirty = false;
        }
    }

    #[test]
    fn submitted_damage_uses_draw_updated_geometry_before_dirty_clear() {
        let mut widget = DrawUpdatedDamage {
            damage: KurboRect::new(0.0, 0.0, 20.0, 8.0),
            dirty: true,
        };
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::default(),
        );

        let mut regions = Vec::new();
        collect_submitted_widget_damage(&widget, &mut regions);
        assert_eq!(regions, vec![KurboRect::new(80.0, 0.0, 100.0, 8.0)]);
        assert!(widget.dirty, "collection must not consume the dirty flag");

        widget.mark_drawn();
        assert!(!widget.dirty);
    }
}

#[cfg(test)]
mod wake_integration_tests {
    use super::*;
    use crate::view::{FreezeThaw, ViewContribution};
    use crate::wake::{WakeEvent, WakeHandle, WakeScope};
    use crate::{LatestRequest, LocalState};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc::{self, Receiver, TryRecvError};

    fn bound_scope() -> (WakeScope, Receiver<WakeEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut scope = WakeScope::new();
        scope
            .bind(move |event| {
                let _ = tx.send(event);
            })
            .unwrap();
        (scope, rx)
    }

    type Captures = Rc<RefCell<Vec<(&'static str, LatestRequest<u32>)>>>;

    struct CaptureRoot {
        captures: Captures,
    }

    impl CaptureRoot {
        fn capture(&self, label: &'static str) {
            self.captures
                .borrow_mut()
                .push((label, LatestRequest::new()));
        }
    }

    impl Drop for CaptureRoot {
        fn drop(&mut self) {
            self.capture("drop");
        }
    }

    impl Widget for CaptureRoot {
        fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {
            self.capture("sync");
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.capture("layout");
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}

        fn draw(
            &mut self,
            _: &mut vello::Scene,
            _: vello::kurbo::Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }

        #[allow(
            clippy::too_many_arguments,
            reason = "signature is fixed by the Widget trait"
        )]
        fn handle_event(
            &mut self,
            _: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            _: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
            self.capture("event");
        }

        fn name(&self) -> &'static str {
            "capture-root"
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }

        fn view_state(&self) -> Option<ViewContribution> {
            self.capture("capture-view");
            Some(ViewContribution::default())
        }

        fn restore_view_state(&mut self, _: &ViewContribution) {
            self.capture("restore-view");
        }
    }

    #[test]
    fn factory_state_stays_owned_before_binding_and_after_return() {
        let mut captured = None;
        let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
            let state = LocalState::new(0);
            state.writer().set(1);
            captured = Some(state);
            Box::new(crate::widget::Text::new("root"))
        });
        let state = captured.unwrap();
        let (tx, rx) = mpsc::channel();
        app.wake_scope
            .as_mut()
            .unwrap()
            .bind(move |event| {
                let _ = tx.send(event);
            })
            .unwrap();
        let pending = rx.try_recv().unwrap();
        app.needs_sync = false;
        assert!(app.accept_wake(&pending));
        assert!(app.needs_sync);
        assert_eq!(state.get(), 1);
        drop(app);
        assert!(!state.writer().try_set(2));
    }

    #[test]
    fn transfer_preserves_the_exact_owner_and_rejects_foreign_events() {
        let (scope, rx) = bound_scope();
        let (other, other_rx) = bound_scope();
        let state = {
            let _entry = scope.enter();
            LocalState::new(0)
        };
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            scope,
            State::new(),
            Box::new(crate::widget::Text::new("root")),
        );
        state.writer().set(1);
        let current = rx.try_recv().unwrap();
        other.handle().wake();
        let foreign = other_rx.try_recv().unwrap();
        app.needs_sync = false;
        assert!(!app.accept_wake(&foreign));
        assert!(!app.needs_sync);
        assert!(app.accept_wake(&current));
        assert!(app.needs_sync);
        app.close_wake_scope();
        app.needs_sync = false;
        assert!(!app.accept_wake(&current));
        assert!(!app.needs_sync);
    }

    #[test]
    fn fallible_factory_error_closes_captures_and_restores_parent_context() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let mut captured = None;
        let result: Result<App, &'static str> = App::try_from_factory(State::new(), || {
            captured = Some(LocalState::new(0));
            Err("factory failed")
        });
        assert!(matches!(result, Err("factory failed")));
        assert!(!captured.unwrap().writer().try_set(1));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    fn factory_unwind_closes_captures_and_restores_parent_context() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let mut captured = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            App::from_factory(State::new(), || {
                captured = Some(LocalState::new(0));
                panic!("factory panicked");
            })
        }));
        assert!(result.is_err());
        assert!(!captured.unwrap().writer().try_set(1));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    fn callback_created_requests_keep_app_owner_when_begun_elsewhere() {
        let (scope, rx) = bound_scope();
        let (other, other_rx) = bound_scope();
        let captures = Captures::default();
        let root = {
            let _entry = scope.enter();
            Box::new(CaptureRoot {
                captures: Rc::clone(&captures),
            })
        };
        let mut app = App::from_scoped_parts_with_controlled_inputs(scope, State::new(), root);
        {
            let _other = other.enter();
            app.init_layout();
            app.refresh_focus_order();
            let view = app.capture_view();
            app.restore_view(&view);
        }
        let requests = std::mem::take(&mut *captures.borrow_mut());
        for label in ["layout", "sync", "capture-view", "restore-view"] {
            assert!(requests.iter().any(|(actual, _)| *actual == label));
        }
        for (_, mut request) in requests {
            let completion = {
                let _other = other.enter();
                request.begin()
            };
            assert!(
                std::thread::spawn(move || completion.complete(7))
                    .join()
                    .unwrap()
            );
            assert!(app.accept_wake(&rx.try_recv().unwrap()));
            assert_eq!(request.try_take(), Some(7));
        }
        assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
        {
            let _other = other.enter();
            drop(app);
        }
        let mut after_drop = std::mem::take(&mut *captures.borrow_mut());
        assert!(after_drop.iter().any(|(label, _)| *label == "drop"));
        for (_, request) in &mut after_drop {
            assert!(request.try_begin().is_none());
        }
        assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn detached_app_callbacks_mask_an_ambient_host() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let captures = Captures::default();
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CaptureRoot {
                captures: Rc::clone(&captures),
            }),
        );
        app.refresh_focus_order();
        let mut requests = std::mem::take(&mut *captures.borrow_mut());
        assert!(!requests.is_empty());
        for (_, request) in &mut requests {
            assert!(request.begin().complete(1));
            assert_eq!(request.try_take(), Some(1));
        }
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    #[should_panic(expected = "live App requires from_factory")]
    fn detached_native_startup_fails_before_creating_an_event_loop() {
        App::new_with_controlled_inputs(State::new(), Box::new(crate::widget::Text::new("root")))
            .run();
    }
}

#[cfg(test)]
mod bound_component_tests {
    use super::*;
    use crate::application::{
        AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
    };
    use crate::widget::{AppKind, AppWidget, Column, IconDescriptor, TileHost};
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    use std::cell::RefCell;
    use std::rc::Rc;
    thread_local! { static CAPTURES: RefCell<Vec<(&'static str, crate::LatestRequest<u32>)>> = const { RefCell::new(vec![]) }; }
    thread_local! { static DISCOVERY_COUNT: Cell<usize> = const { Cell::new(0) }; }
    thread_local! { static CONTROL_AUTHORITY: RefCell<Vec<(bool, bool)>> = const { RefCell::new(vec![]) }; }
    thread_local! { static COMPOUND_MODE: Cell<u8> = const { Cell::new(0) }; }
    thread_local! {
        static PAGED_SEARCH_COUNTS: RefCell<Option<Rc<[Cell<usize>; 2]>>> = const { RefCell::new(None) };
    }
    struct PagedProbe {
        catalog: crate::search::SearchCatalog,
        resolutions: Rc<[Cell<usize>; 2]>,
        owner: usize,
    }
    impl PagedProbe {
        fn new(strict: bool, resolutions: Rc<[Cell<usize>; 2]>) -> Self {
            let catalog = crate::search::SearchCatalog::new("Bound paging probe");
            for id in 0..65 {
                let resource = format!("bound:paged:{id}");
                catalog
                    .upsert(
                        &resource,
                        "paged",
                        crate::search::SearchResult::for_action(
                            format!("Paged kind {id}"),
                            format!("{} paged {id}", if strict { "Allowed" } else { "Revoked" }),
                            tilo_actions::ActionAddress::new(&resource, "apply"),
                        ),
                    )
                    .unwrap();
            }
            Self {
                catalog,
                resolutions,
                owner: usize::from(strict),
            }
        }
    }
    impl crate::search::SearchProvider for PagedProbe {
        fn name(&self) -> &'static str {
            "Bound paging probe"
        }
        fn search(&self, _: &str) -> Vec<crate::search::SearchResult> {
            panic!("the controlled paging fixture must use search_page")
        }
        fn search_page(
            &self,
            query: &str,
            cursor: Option<&crate::search::SearchCursor>,
            work_budget: usize,
            result_limit: usize,
        ) -> crate::search::SearchPage {
            self.catalog
                .query_page(query, cursor, work_budget, result_limit)
        }
        // Deliberately inherit unknown generation: validation cannot cache a
        // claim that this provider's authorization declarations are stable.
        fn resolve_action_resource(
            &self,
            address: &tilo_actions::ActionAddress,
        ) -> Option<tilo_actions::Resource> {
            let counter = &self.resolutions[self.owner];
            counter.set(counter.get().saturating_add(1));
            address.resource.strip_prefix("bound:paged:")?;
            (address.action == "apply").then(|| {
                tilo_actions::Resource::new(&address.resource, "probe", "Paged").with_action(
                    tilo_actions::ActionSpec::immediate("apply", "Apply", "bound.apply", &FollowUp)
                        .unwrap()
                        .requiring(tilo_actions::PermissionRequirement::Write),
                )
            })
        }
    }
    #[derive(Default)]
    struct Probe {
        strict: bool,
        collision: Option<String>,
        actions: crate::action_host::ActionHost,
    }
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct FollowUp;
    impl tilo_actions::Action for FollowUp {
        fn apply(&self, doc: &mut automerge::AutoCommit) -> Result<(), tilo_actions::ActionError> {
            doc.put(ROOT, "follow-up", true)?;
            Ok(())
        }
    }
    #[derive(Debug, serde::Serialize)]
    struct EmptyFollowUp {
        refuse: bool,
    }
    impl tilo_actions::Action for EmptyFollowUp {
        fn apply(&self, doc: &mut automerge::AutoCommit) -> Result<(), tilo_actions::ActionError> {
            if self.refuse {
                doc.put(ROOT, "failed-action", true)?;
                Err(tilo_actions::ActionError::Refused(
                    "controlled failure".into(),
                ))
            } else {
                Ok(())
            }
        }
    }
    impl Probe {
        fn capture(label: &'static str) {
            CAPTURES.with_borrow_mut(|v| v.push((label, crate::LatestRequest::new())));
        }
    }

    struct LazyProbe {
        strict: bool,
    }
    impl crate::search::SearchProvider for LazyProbe {
        fn name(&self) -> &'static str {
            "Probe lazy"
        }
        fn action_directory_generation(&self) -> Option<u64> {
            Some(0)
        }
        fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
            match query {
                "sealed" => vec![crate::search::SearchResult::for_action(
                    "Probe",
                    if self.strict {
                        "Allowed sealed sibling title"
                    } else {
                        "Hidden sealed write title"
                    },
                    tilo_actions::ActionAddress::new("bound:sealed", "apply"),
                )],
                "sealedopaque" => vec![crate::search::SearchResult::new(
                    "Probe",
                    if self.strict {
                        "Allowed sealedopaque title"
                    } else {
                        "Hidden sealedopaque title"
                    },
                    "opaque",
                )],
                "lazy" => vec![crate::search::SearchResult::for_action(
                    "Probe",
                    "Lazy",
                    tilo_actions::ActionAddress::new("bound:lazy", "view"),
                )],
                "opaque" => vec![crate::search::SearchResult::new(
                    "Probe", "Opaque", "opaque",
                )],
                _ => vec![],
            }
        }
        fn resolve_action_resource(
            &self,
            address: &tilo_actions::ActionAddress,
        ) -> Option<tilo_actions::Resource> {
            if address.resource == "bound:sealed" && address.action == "apply" {
                return Some(
                    tilo_actions::Resource::new("bound:sealed", "probe", "Sealed").with_action(
                        tilo_actions::ActionSpec::immediate(
                            "apply",
                            "Apply",
                            "bound.apply",
                            &FollowUp,
                        )
                        .unwrap()
                        .requiring(tilo_actions::PermissionRequirement::Write),
                    ),
                );
            }
            (address.resource == "bound:lazy" && address.action == "view").then(|| {
                tilo_actions::Resource::new("bound:lazy", "probe", "Lazy").with_action(
                    tilo_actions::ActionSpec::operation(
                        "view",
                        "View",
                        tilo_actions::OperationKind::View,
                        "bound.view",
                        &(),
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                )
            })
        }
    }
    impl Widget for Probe {
        fn refresh_control_action_availability(
            &mut self,
            allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
        ) {
            CONTROL_AUTHORITY.with_borrow_mut(|seen| {
                seen.push((
                    self.strict,
                    allowed.contains(&tilo_actions::ActionAddress::new("bound:probe", "apply")),
                ));
            });
        }

        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            if let Some(counts) = PAGED_SEARCH_COUNTS.with_borrow(Clone::clone) {
                return vec![Box::new(PagedProbe::new(self.strict, counts))];
            }
            vec![Box::new(LazyProbe {
                strict: self.strict,
            })]
        }
        fn activate_search_result(
            &mut self,
            id: &str,
            doc: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
        ) {
            assert_eq!(id, "opaque");
            doc.put(ROOT, "navigation", true).unwrap();
        }

        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &["bound.follow-up", "bound.view"]
        }
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
            DISCOVERY_COUNT.set(DISCOVERY_COUNT.get().saturating_add(1));
            let mut action = tilo_actions::ActionSpec::operation(
                "follow-up",
                "Follow up",
                tilo_actions::OperationKind::Effect,
                "bound.follow-up",
                &(),
            )
            .unwrap();
            action.contract.execution =
                tilo_actions::ExecutionMode::Background { cancellable: true };
            action.contract.result = tilo_actions::ResultSpec::Job;
            if self.strict {
                action.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
            }
            vec![
                tilo_actions::Resource::new(
                    self.collision.as_deref().unwrap_or("bound:probe"),
                    "probe",
                    "Probe",
                )
                .with_action(action)
                .with_action(
                    tilo_actions::ActionSpec::immediate(
                        "apply",
                        "Apply follow up",
                        "bound.apply",
                        &FollowUp,
                    )
                    .unwrap(),
                )
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "view",
                        "View",
                        tilo_actions::OperationKind::View,
                        "bound.view",
                        &(),
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                ),
            ]
        }
        fn resource(
            &self,
            doc: &automerge::AutoCommit,
            id: &str,
        ) -> Option<tilo_actions::Resource> {
            self.resources(doc)
                .into_iter()
                .find(|resource| resource.id.as_str() == id)
        }
        fn action_directory_visibility(
            &self,
            _: &automerge::AutoCommit,
            id: &str,
        ) -> Option<tilo_actions::ActionAddress> {
            (self.strict && id == "bound:probe")
                .then(|| tilo_actions::ActionAddress::new(id, "follow-up"))
        }
        fn action_directory_resource_page(
            &self,
            doc: &automerge::AutoCommit,
            cursor: &[u8],
            max_results: usize,
        ) -> Option<crate::widget::ActionDirectoryResourcePage> {
            Some(crate::widget::ActionDirectoryResourcePage {
                resources: if cursor.is_empty() && max_results > 0 {
                    self.resources(doc)
                } else {
                    vec![]
                },
                next_cursor: vec![],
                exhausted: true,
            })
        }
        #[allow(
            clippy::panic_in_result_fn,
            reason = "probe asserts canonical admission before returning the toolkit result"
        )]
        fn start_resource_operation_with_document(
            &mut self,
            _: tilo_actions::InvocationId,
            _: tilo_actions::OperationKind,
            type_id: &str,
            _: &[u8],
            doc: &mut automerge::AutoCommit,
            path: &automerge::ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            if type_id == "bound.view" {
                if COMPOUND_MODE.get() == 1 {
                    self.actions.dispatch(doc, "bound.apply", &FollowUp);
                }
                Self::capture("bound-view");
                return Ok(Some(crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )));
            }
            assert_eq!(type_id, "bound.follow-up");
            assert_eq!(path, &ROOT);
            assert_eq!(
                doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            let mode = COMPOUND_MODE.get();
            if matches!(mode, 3 | 4 | 6) {
                doc.put(ROOT, "raw-before", true).unwrap();
            }
            match mode {
                2 | 3 => {}
                7 => self.actions.dispatch_with_context(
                    doc,
                    "bound.apply",
                    &FollowUp,
                    tilo_actions::ActionContext::for_resource(
                        "Declared follow-up",
                        "bound:probe",
                        "probe",
                        "Probe",
                    ),
                ),
                8 => {
                    let mut queue = tilo_actions::OptimisticQueue::new(doc);
                    self.actions
                        .dispatch_optimistic(doc, &mut queue, "bound.apply", &FollowUp)
                        .unwrap();
                }
                9 | 10 => {
                    assert_eq!(
                        self.actions
                            .try_dispatch(doc, "bound.empty", &EmptyFollowUp { refuse: mode == 10 })
                            .is_err(),
                        mode == 10
                    );
                }
                11 => {
                    doc.put(ROOT, "raw-unwind", true).unwrap();
                    panic!("controlled compound unwind");
                }
                _ => self.actions.dispatch(doc, "bound.apply", &FollowUp),
            }
            if matches!(mode, 5 | 6) {
                doc.put(ROOT, "raw-after", true).unwrap();
            }
            Self::capture("mutable-operation");
            Ok(Some(crate::widget::ResourceOperationStart::Accepted))
        }
        fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
            self.actions.take_committed()
        }
        fn poll_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            cancellation_requested: bool,
            doc: &automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> Option<tilo_actions::InvocationUpdate> {
            if !cancellation_requested {
                return None;
            }
            assert_eq!(
                doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            assert!(!crate::local_state::LocalState::new(0).writer().try_set(1));
            Self::capture("mutable-cancel");
            Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: None,
            })
        }

        fn sync(&mut self, doc: &mut automerge::AutoCommit, _: &automerge::ObjId) {
            assert_eq!(
                doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            Self::capture("sync");
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            Self::capture("build");
            let node = tree.new_leaf(taffy::Style::default()).unwrap();
            map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn measure(
            &mut self,
            _: taffy::Size<Option<f32>>,
            _: taffy::Size<taffy::AvailableSpace>,
            _: &mut TextRenderer,
        ) -> taffy::Size<f32> {
            Self::capture("measure");
            taffy::Size {
                width: 30.0,
                height: 20.0,
            }
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {
            Self::capture("apply");
        }
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
            Self::capture("draw");
        }
        fn handle_event(
            &mut self,
            _: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            d: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
            assert_eq!(
                d.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            Self::capture("event");
        }
        fn name(&self) -> &'static str {
            "bound-probe"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
        fn is_focusable(&self) -> bool {
            Self::capture("focus-order");
            true
        }
        fn set_focus(&mut self, _: bool) {
            Self::capture("focus");
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Self::capture("accessibility");
            Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name("Child"))
        }
        fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
            Self::capture("ime");
            Some(winit::window::ImePurpose::Normal)
        }
        fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
            Self::capture("ime-rect");
            Some(vello::kurbo::Rect::ZERO)
        }
        fn perform_action(
            &mut self,
            _: &crate::a11y::Action,
            doc: &mut automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> bool {
            assert_eq!(
                doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
                Some("child")
            );
            doc.put(ROOT, "action", true).unwrap();
            Self::capture("action");
            true
        }
    }
    impl AppWidget for Probe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Probe"
        }
        fn app_icon(&self) -> IconDescriptor {
            IconDescriptor::Glyph("P")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }
    #[allow(clippy::needless_pass_by_value, reason = "factory contract")]
    fn factory(
        context: HostContext,
        target: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, crate::launch::LaunchError> {
        build_probe(context, &target, false)
    }
    #[allow(clippy::needless_pass_by_value, reason = "factory contract")]
    fn strict_factory(
        context: HostContext,
        target: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, crate::launch::LaunchError> {
        build_probe(context, &target, true)
    }
    fn build_probe(
        mut context: HostContext,
        target: &LaunchTarget,
        strict: bool,
    ) -> Result<AppComponent, crate::launch::LaunchError> {
        if matches!(target, LaunchTarget::New) {
            context
                .initialize_new_record(|doc| {
                    doc.put(ROOT, "marker", "child").map_err(|e| e.to_string())
                })
                .map_err(crate::launch::LaunchError::Record)?;
        }
        let mut registry = tilo_actions::ActionRegistry::new();
        registry.register::<FollowUp>("bound.apply");
        Ok(AppComponent::new(
            context,
            Box::new(Probe {
                strict,
                collision: None,
                actions: crate::action_host::ActionHost::new(),
            }),
            registry,
        ))
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one actual App lifetime verifies descendant delivery, mutable admission and closed cancellation"
    )]
    fn real_app_descendant_focus_ime_semantics_and_measure_keep_child_binding() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut owners = crate::record::RecordOwners::new(root.clone());
        let records = owners.open("so.tilo.binding-probe", "A").unwrap();
        let views =
            Rc::new(crate::application_view::InstanceViewStore::new(root, "D", "A").unwrap());
        let definition = ApplicationDefinition {
            app_id: "so.tilo.binding-probe",
            title: "Probe",
            icon: IconDescriptor::Glyph("P"),
            record_kind: crate::record::RecordKind::new("probe"),
            required: vec![],
            optional: vec![],
            create: factory,
        };
        let context = HostContext::prepare(
            &definition,
            records.clone(),
            views,
            "D".into(),
            1,
            Rc::new(RefCell::new(
                crate::permissions::PermissionContext::permissive(),
            )),
            crate::permissions::UserId("probe-author".into()),
            tilo_actions::ActionContext::default(),
            &LaunchTarget::New,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let checkout = component.record().clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        component
            .mount(move |event| {
                sender.send(event).unwrap();
            })
            .unwrap();
        let mut outer = crate::wake::WakeScope::new();
        let (tx, rx) = std::sync::mpsc::channel();
        outer
            .bind(move |event| {
                tx.send(event).unwrap();
            })
            .unwrap();
        let mut state = State::new();
        state.doc.put(ROOT, "marker", "shell").unwrap();
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            outer,
            state,
            Box::new(
                Column::new().push(Box::new(
                    crate::widget::Row::new().push(Box::new(
                        crate::widget::Container::new()
                            .with_child(Box::new(TileHost::from_component(component))),
                    )),
                )),
            ),
        );
        CAPTURES.with_borrow_mut(Vec::clear);
        app.init_layout();
        app.refresh_focus_order();
        let leaf = app.focusable_order[0];
        app.set_focused_node(Some(leaf));
        let _ = app.ime_window_state();
        app.dispatch_a11y_action(
            accesskit::NodeId(u64::from(leaf).saturating_add(1)),
            &crate::a11y::Action::Focus,
        );
        let layout_root = app.layout.root_node.unwrap();
        app.layout.layout_dirty = true;
        run_layout_pass(&mut app, layout_root, 400, 300, 1.0, true, None).unwrap();
        assert!(
            tilo_actions::door::resolve(
                &app.collect_local_resources(),
                &tilo_actions::ActionAddress::new("bound:probe", "follow-up")
            )
            .is_some(),
            "fixture must publish its operation before admission"
        );
        let actor = app.collaboration.local_author.0.clone();
        assert!(matches!(
            app.invoke_resource_action(
                &tilo_actions::ActionAddress::new("bound:probe", "follow-up"),
                None,
                &actor,
                false,
                false,
                None,
                Some(tilo_actions::InvocationId::new()),
                None,
            )
            .unwrap(),
            crate::widget::ResourceOperationStart::Accepted
        ));
        assert!(
            records
                .read(&checkout.id, |doc| doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .unwrap()
                    .0
                    .to_bool()
                    .unwrap())
                .unwrap()
        );
        assert!(app.state.doc.get(ROOT, "follow-up").unwrap().is_none());
        let captures = CAPTURES.with_borrow_mut(std::mem::take);
        assert_eq!(
            captures
                .iter()
                .filter(|(label, _)| *label == "mutable-operation")
                .count(),
            1
        );
        for label in [
            "build",
            "sync",
            "measure",
            "apply",
            "focus-order",
            "focus",
            "ime",
            "accessibility",
            "action",
        ] {
            assert!(
                captures.iter().any(|(actual, _)| *actual == label),
                "missing {label}"
            );
        }
        for (label, mut request) in captures {
            assert!(request.begin().complete(1));
            assert!(receiver.try_recv().is_ok(), "{label} missed child");
            assert!(rx.try_recv().is_err(), "{label} woke outer");
        }
        let close_probe = *app.actions.bound_invocations.keys().next().unwrap();
        app.actions.local_invocations.insert(
            close_probe,
            (
                tilo_actions::ActionAddress::new("bound:probe", "follow-up"),
                tilo_actions::OperationKind::Effect,
            ),
        );
        app.close_admission.save_waiting = true;
        app.close_admission.save_local = Some(close_probe);
        app.close_components();
        // The target was revoked while its canonical operation was Accepted.
        // The actual poll path must turn refusal into recoverable close UI.
        app.poll_directed_invocations();
        assert!(matches!(app.close_admission.save_result, Some(Err(_))));
        app.service_close_admission();
        assert!(!app.close_admission.save_waiting);
        assert!(!app.close_admission.admitted);
        assert!(app.close_admission.dialog.is_some());
        assert_eq!(
            CAPTURES.with_borrow(|c| c
                .iter()
                .filter(|(label, _)| *label == "mutable-cancel")
                .count()),
            1
        );
        assert_eq!(app.component_close_reports().len(), 1);
        app.close_components();
        assert_eq!(app.component_close_reports().len(), 1);
        assert_eq!(app.take_component_close_reports().len(), 1);
        assert!(app.take_component_close_reports().is_empty());
    }
    struct ActionFixture {
        _temp: tempfile::TempDir,
        app: App,
        records: crate::record::RecordStore,
        bindings: Vec<crate::bound_dispatch::CallbackBinding>,
        permissions: Rc<RefCell<crate::permissions::PermissionContext>>,
    }

    #[test]
    fn document_history_keeps_focused_record_after_focus_moves_and_revokes_live() {
        let mut fixture = ActionFixture::new();
        let mut entry_ids = Vec::new();
        for (index, binding) in fixture.bindings.iter().enumerate() {
            let hash = fixture
                .records
                .with(&binding.checkout.id, |doc| {
                    doc.put(ROOT, "history_marker", format!("record-{index}"))
                        .unwrap();
                    doc.commit().unwrap()
                })
                .unwrap();
            entry_ids.push(crate::history_source::entry_id_from_hash(&hash.to_string()));
        }
        fixture
            .app
            .state
            .doc
            .put(ROOT, "history_marker", "outer")
            .unwrap();
        let pending = fixture.app.state.doc.pending_ops();
        let focus_for = |app: &App, instance| {
            app.layout
                .node_map
                .keys()
                .copied()
                .find(|node| {
                    app.bindings
                        .binding_for(*node)
                        .is_some_and(|binding| binding.instance_id == instance)
                })
                .unwrap()
        };
        fixture.app.focused_node = Some(focus_for(&fixture.app, fixture.bindings[0].instance_id));
        fixture.app.open_document_history().unwrap();
        fixture.app.focused_node = Some(focus_for(&fixture.app, fixture.bindings[1].instance_id));
        fixture
            .app
            .select_document_history(&[entry_ids[0]])
            .unwrap();
        let snapshot = fixture.app.document_history_snapshot().unwrap().unwrap();
        let after =
            automerge::AutoCommit::load(&snapshot.comparison.unwrap().after_document).unwrap();
        assert_eq!(
            after
                .get(ROOT, "history_marker")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some("record-0")
        );
        assert!(
            fixture
                .app
                .select_document_history(&[entry_ids[1]])
                .is_err()
        );
        assert_eq!(fixture.app.state.doc.pending_ops(), pending);
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
        assert!(fixture.app.document_history_snapshot().is_err());
        assert!(!fixture.app.document_history.is_open());
    }

    #[test]
    fn document_history_does_not_fall_back_when_component_closes() {
        let mut fixture = ActionFixture::new();
        let binding = &fixture.bindings[0];
        fixture.app.focused_node = fixture.app.layout.node_map.keys().copied().find(|node| {
            fixture
                .app
                .bindings
                .binding_for(*node)
                .is_some_and(|candidate| candidate.instance_id == binding.instance_id)
        });
        assert!(fixture.app.focused_node.is_some());
        fixture.app.open_document_history().unwrap();
        binding.closing.set(true);
        fixture.app.focused_node = None;
        assert!(fixture.app.document_history_snapshot().is_err());
        assert!(!fixture.app.document_history.is_open());
    }

    #[test]
    fn document_history_requires_a_choice_when_multiple_components_have_no_focus() {
        let mut fixture = ActionFixture::new();
        fixture.app.focused_node = None;
        assert!(fixture.app.open_document_history().is_err());
        assert!(fixture.app.document_history_snapshot().unwrap().is_none());
    }

    impl ActionFixture {
        fn new() -> Self {
            Self::with_same_record(false)
        }
        fn with_same_record(shared: bool) -> Self {
            Self::with_authority(shared, false)
        }
        fn with_authority(shared: bool, separate: bool) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let mut owners = crate::record::RecordOwners::new(temp.path().into());
            let records = owners
                .open(
                    "so.tilo.binding-probe",
                    "00000000-0000-0000-0000-000000000004",
                )
                .unwrap();
            let views = Rc::new(
                crate::application_view::InstanceViewStore::new(
                    temp.path().into(),
                    "D",
                    "00000000-0000-0000-0000-000000000004",
                )
                .unwrap(),
            );
            let permissions = Rc::new(RefCell::new(
                crate::permissions::PermissionContext::from_capabilities(
                    crate::permissions::Actor::person(None, "fixture-user"),
                    vec![
                        crate::permissions::CapOverride::grant(
                            crate::permissions::Subject::person("fixture-user"),
                            crate::permissions::Scope::Workspace,
                            crate::permissions::Verb::Admin,
                        )
                        .with_expiry(110),
                    ],
                )
                .at_unix_time(100),
            ));
            let mut column = Column::new();
            let mut bindings: Vec<crate::bound_dispatch::CallbackBinding> = Vec::new();
            for (instance, create) in [
                (11, factory as crate::application::ComponentFactory),
                (12, strict_factory),
            ] {
                let definition = ApplicationDefinition {
                    app_id: "so.tilo.binding-probe",
                    title: "Probe",
                    icon: IconDescriptor::Glyph("P"),
                    record_kind: crate::record::RecordKind::new("probe"),
                    required: vec![],
                    optional: vec![],
                    create,
                };
                let target = if shared && !bindings.is_empty() {
                    LaunchTarget::Record {
                        checkout: bindings[0].checkout.clone(),
                        reveal: None,
                    }
                } else {
                    LaunchTarget::New
                };
                let context = HostContext::prepare(
                    &definition,
                    records.clone(),
                    views.clone(),
                    "D".into(),
                    instance,
                    if separate && instance == 12 {
                        Rc::new(RefCell::new(permissions.borrow().clone()))
                    } else {
                        permissions.clone()
                    },
                    crate::permissions::UserId("fixture-user".into()),
                    tilo_actions::ActionContext::default(),
                    &target,
                )
                .unwrap();
                let component = definition
                    .instantiate(context, target, RestoreInput::None)
                    .unwrap();
                bindings.push(component.callback_binding());
                column = column.push(Box::new(TileHost::from_component(component)));
            }
            let mut app = App::new_with_controlled_inputs(State::new(), Box::new(column));
            app.init_layout();
            Self {
                _temp: temp,
                app,
                records,
                bindings,
                permissions,
            }
        }
        fn broker() -> Self {
            let mut fixture = Self::with_same_record(true);
            fixture.app.actions.bound_broker_route_for_test =
                Some(crate::collaboration::PrimaryRouteWitness {
                    route: fixture.bindings[0].broker_route().unwrap(),
                    epoch: uuid::Uuid::new_v4(),
                });
            fixture
        }
        fn target(&self, index: usize, action: &str) -> crate::bound_dispatch::BoundActionTarget {
            let binding = self.bindings[index].clone();
            crate::bound_dispatch::BoundActionTarget {
                address: crate::bound_dispatch::BoundActionAddress {
                    instance_id: binding.instance_id,
                    address: tilo_actions::ActionAddress::new("bound:probe", action),
                },
                binding,
            }
        }
    }

    include!("bound_record_sync_tests.rs");

    #[test]
    fn task4_bound_publication_preserves_contracts_and_directed_owner() {
        let mut fixture = ActionFixture::broker();
        let outer = fixture.app.state.doc.get_heads();
        let address = tilo_actions::ActionAddress::new("bound:probe", "follow-up");
        let actor = fixture.app.collaboration.local_author.0.clone();
        let error = fixture
            .app
            .invoke_resource_action(&address, None, &actor, true, false, None, None, None)
            .unwrap_err();
        assert_eq!(
            crate::bound_dispatch::door_error_code(&error),
            "ambiguous_action_target"
        );
        let resources = fixture.app.publication_resources();
        let first = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[0], "bound:probe")
            .unwrap();
        let second = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        assert_ne!(first, second);
        let descriptor = |id: &str| {
            tilo_actions::door::resolve(
                &resources,
                &tilo_actions::ActionAddress::new(id, "follow-up"),
            )
            .unwrap()
        };
        assert_eq!(
            descriptor(&first).contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        );
        assert_eq!(
            descriptor(&second).contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        let revision = fixture.app.action_directory_revision();
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &second, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Found(_)
        ));
        fixture
            .app
            .invoke_resource_action(
                &tilo_actions::ActionAddress::new(&second, "follow-up"),
                None,
                &actor,
                true,
                true,
                Some(&serde_json::json!({"confirm": true})),
                Some(tilo_actions::InvocationId::new()),
                None,
            )
            .unwrap();
        assert!(
            fixture
                .records
                .read_history(&fixture.bindings[0].checkout.id, |_, history| history
                    .next_undo_in(&fixture.bindings[0].author, 11)
                    .is_none())
                .unwrap()
        );
        fixture
            .records
            .with_history(&fixture.bindings[1].checkout.id, |doc, history| {
                assert!(doc.get(ROOT, "follow-up").unwrap().is_some());
                assert_eq!(
                    history
                        .next_undo_in(&fixture.bindings[1].author, 12)
                        .unwrap()
                        .instance_id,
                    Some(12)
                );
            })
            .unwrap();
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
    }

    #[test]
    fn task4_bound_cache_tracks_permission_time_and_never_reuses_exhaustion() {
        let mut fixture = ActionFixture::new();
        DISCOVERY_COUNT.set(0);
        let _ = fixture.app.collect_resources();
        let first = DISCOVERY_COUNT.get();
        let _ = fixture.app.collect_resources();
        assert_eq!(DISCOVERY_COUNT.get(), first);
        let expired = fixture.permissions.borrow().clone().at_unix_time(120);
        *fixture.permissions.borrow_mut() = expired;
        let resources = fixture.app.collect_resources();
        assert!(
            tilo_actions::door::resolve(
                &resources,
                &tilo_actions::ActionAddress::new("bound:probe", "follow-up")
            )
            .is_none()
        );
        assert!(DISCOVERY_COUNT.get() > first);
        let expired = fixture.permissions.borrow().clone();
        *fixture.permissions.borrow_mut() = expired.at_unix_time(100);
        fixture
            .records
            .with_action_state(&fixture.bindings[0].checkout.id, |_, _, state| {
                state.generation = Some(u64::MAX);
            })
            .unwrap();
        fixture.bindings[0]
            .with_document(&mut fixture.app.state.doc, |_, _| {})
            .unwrap();
        let _ = fixture.app.collect_resources();
        let before = DISCOVERY_COUNT.get();
        let _ = fixture.app.collect_resources();
        assert!(DISCOVERY_COUNT.get() > before);
        assert_eq!(
            fixture
                .records
                .action_generation(&fixture.bindings[0].checkout.id)
                .unwrap(),
            None
        );
    }

    #[test]
    fn task4_semantic_and_palette_targets_keep_equal_id_lifetimes() {
        let mut fixture = ActionFixture::new();
        fixture.app.refresh_semantic_tree();
        let targets = fixture
            .app
            .accessibility
            .semantic_tree
            .bound_actions_for_test();
        let (id, _) = targets
            .iter()
            .find(|(_, target)| {
                target.address.instance_id == 12 && target.address.address.action == "view"
            })
            .unwrap();
        let outer = fixture.app.state.doc.get_heads();
        assert!(
            fixture
                .app
                .invoke_semantic_action(id, "view", None)
                .unwrap()
        );
        fixture.app.open_action_palette();
        let target = fixture.target(0, "view");
        assert!(
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .activate_bound(&target)
        );
        let pending = fixture
            .app
            .actions
            .action_palette
            .as_mut()
            .unwrap()
            .take_operation()
            .unwrap();
        assert!(
            pending
                .bound
                .as_ref()
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
        fixture.app.close_components();
        assert!(
            fixture
                .app
                .invoke_bound_target(
                    &target,
                    None,
                    &fixture.app.collaboration.local_author.0.clone(),
                    true,
                    false,
                    None,
                    None,
                    None
                )
                .is_err()
        );
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one controlled lifetime traces publication through refusal and delivery"
    )]
    fn task4_bound_directory_limits_cursors_and_alias_refusal() {
        let mut fixture = ActionFixture::broker();
        let query = runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: String::new(),
            cursor: vec![],
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 8192,
            max_work: 16,
            max_depth: 8,
        };
        let first = fixture.app.action_directory_page(query.clone());
        assert_eq!(first.rows.len(), 1);
        assert!(!first.next_cursor.is_empty());
        let second = fixture
            .app
            .action_directory_page(runtime_collab::ActionDirectoryQuery {
                revision: first.revision.clone(),
                cursor: first.next_cursor.clone(),
                ..query.clone()
            });
        assert_eq!(second.rows.len(), 1);
        assert_ne!(first.rows[0].resource, second.rows[0].resource);
        assert_eq!(first.rows[0].visibility_action.as_deref(), Some("view"));
        assert_eq!(
            second.rows[0].visibility_action.as_deref(),
            Some("follow-up")
        );
        let eager = fixture.app.publication_resources();
        for page in [&first, &second] {
            assert!(
                eager
                    .iter()
                    .any(|resource| resource.id.as_str() == page.rows[0].resource)
            );
        }
        let alias = second.rows[0].resource.clone();
        let previous_permissions = fixture.permissions.borrow().clone();
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
        let stale = fixture
            .app
            .action_directory_page(runtime_collab::ActionDirectoryQuery {
                revision: first.revision.clone(),
                cursor: first.next_cursor,
                ..query
            });
        assert!(stale.stale_cursor);
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_none()
        );
        let actor = fixture.app.collaboration.local_author.0.clone();
        assert!(
            fixture
                .app
                .invoke_resource_action(
                    &tilo_actions::ActionAddress::new(&alias, "follow-up"),
                    None,
                    &actor,
                    true,
                    true,
                    None,
                    None,
                    None,
                )
                .is_err()
        );
        *fixture.permissions.borrow_mut() = previous_permissions;
        for malformed in [
            format!("{}broken", crate::bound_dispatch::PUBLICATION_PREFIX),
            format!("{alias}:extra"),
            format!(
                "{}{}",
                crate::bound_dispatch::PUBLICATION_PREFIX,
                "x".repeat(4096)
            ),
        ] {
            assert!(
                fixture
                    .app
                    .actions
                    .bound_publication
                    .target(&tilo_actions::ActionAddress::new(&malformed, "view"))
                    .is_err()
            );
            assert!(
                fixture
                    .app
                    .resolve_publication_resource(&malformed, "view")
                    .is_none()
            );
        }
        fixture.app.close_components();
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_none()
        );
        let mut replacement = ActionFixture::broker();
        fixture.app.actions.bound_broker_route_for_test =
            replacement.app.actions.bound_broker_route_for_test.clone();
        fixture.app.root_widget = std::mem::replace(
            &mut replacement.app.root_widget,
            Box::new(crate::widget::Text::new("empty")),
        );
        fixture.app.invalidate_widget_resource_cache();
        let replacements = fixture.app.publication_resources();
        assert!(!replacements.is_empty());
        assert!(
            replacements
                .iter()
                .all(|resource| resource.id.as_str() != alias)
        );
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "view")
                .is_none()
        );
    }

    #[test]
    fn task4_compound_view_and_noop_effect_preserve_pending_typing() {
        for (action, mode) in [("view", 0), ("follow-up", 2), ("view", 1)] {
            COMPOUND_MODE.set(mode);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "first").unwrap();
            })
            .unwrap();
            let pending_before = fixture
                .records
                .read(
                    &a.checkout.id,
                    automerge::transaction::Transactable::pending_ops,
                )
                .unwrap();
            let target = fixture.target(1, action);
            fixture
                .app
                .invoke_bound_target(
                    &target,
                    None,
                    "fixture-user",
                    true,
                    false,
                    None,
                    Some(tilo_actions::InvocationId(uuid::Uuid::from_u128(91))),
                    None,
                )
                .unwrap();
            if mode == 0 {
                assert_eq!(
                    fixture
                        .records
                        .read(
                            &a.checkout.id,
                            automerge::transaction::Transactable::pending_ops
                        )
                        .unwrap(),
                    pending_before
                );
            }
            let b_entry = fixture
                .records
                .read_history(&a.checkout.id, |_, history| {
                    history.next_undo_in(&b.author, b.instance_id).cloned()
                })
                .unwrap();
            assert_eq!(b_entry.is_some(), mode == 1);
            if mode != 1 {
                assert!(
                    fixture
                        .records
                        .read_history(&a.checkout.id, |_, history| history
                            .next_undo_in(&a.author, a.instance_id)
                            .is_none())
                        .unwrap()
                );
            }
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "continued").unwrap();
            })
            .unwrap();
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert_eq!(doc.get(ROOT, "typing").unwrap().is_some(), mode == 1);
                    assert_eq!(doc.get(ROOT, "follow-up").unwrap().is_some(), mode == 1);
                })
                .unwrap();
            COMPOUND_MODE.set(0);
        }
    }

    #[test]
    fn task4_undeclared_effect_spans_are_retained_excluded_and_keep_custody() {
        for mode in [3, 4, 5, 6] {
            COMPOUND_MODE.set(mode);
            CAPTURES.with_borrow_mut(Vec::clear);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "prior").unwrap();
            })
            .unwrap();
            let generation = fixture.records.action_generation(&a.checkout.id).unwrap();
            let target = fixture.target(1, "follow-up");
            let error = fixture
                .app
                .invoke_bound_target(
                    &target,
                    None,
                    "fixture-user",
                    true,
                    false,
                    None,
                    Some(tilo_actions::InvocationId(uuid::Uuid::from_u128(92))),
                    None,
                )
                .unwrap_err();
            assert_eq!(
                crate::bound_dispatch::door_error_code(&error),
                "undeclared_document_mutation"
            );
            assert!(error.to_string().contains("retained"));
            assert_ne!(
                fixture.records.action_generation(&a.checkout.id).unwrap(),
                generation
            );
            b.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "later", true).unwrap();
            })
            .unwrap();
            b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            if mode != 3 {
                b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
            }
            assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert!(doc.get(ROOT, "later").unwrap().is_none());
                    assert!(doc.get(ROOT, "typing").unwrap().is_none());
                    assert!(doc.get(ROOT, "follow-up").unwrap().is_none());
                    assert_eq!(
                        doc.get(ROOT, "raw-before").unwrap().is_some(),
                        matches!(mode, 3 | 4 | 6)
                    );
                    assert_eq!(
                        doc.get(ROOT, "raw-after").unwrap().is_some(),
                        matches!(mode, 5 | 6)
                    );
                })
                .unwrap();
            drop(fixture);
            assert!(CAPTURES.with_borrow(|captures| {
                captures.iter().any(|(name, _)| *name == "mutable-cancel")
            }));
            COMPOUND_MODE.set(0);
        }
    }

    #[test]
    fn task4_compound_contextual_optimistic_noop_and_failed_producers() {
        for mode in [7, 8, 9, 10] {
            COMPOUND_MODE.set(mode);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "prior").unwrap();
            })
            .unwrap();
            let target = fixture.target(1, "follow-up");
            fixture
                .app
                .invoke_bound_target(&target, None, "fixture-user", true, false, None, None, None)
                .unwrap();
            assert_eq!(
                b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo,
                mode < 9
            );
            if mode < 9 {
                b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
                assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
            } else {
                assert!(
                    fixture
                        .records
                        .read_history(&a.checkout.id, |_, history| history
                            .next_undo_in(&a.author, a.instance_id)
                            .is_none())
                        .unwrap()
                );
            }
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert!(doc.get(ROOT, "typing").unwrap().is_none());
                    assert!(doc.get(ROOT, "follow-up").unwrap().is_none());
                    assert!(doc.get(ROOT, "failed-action").unwrap().is_none());
                })
                .unwrap();
            COMPOUND_MODE.set(0);
        }
    }

    #[test]
    fn task4_compound_unwind_restores_frame_and_preserves_unrelated_redo() {
        COMPOUND_MODE.set(11);
        let mut fixture = ActionFixture::with_same_record(true);
        let a = fixture.bindings[0].clone();
        let b = fixture.bindings[1].clone();
        a.with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "typing", "prior").unwrap();
        })
        .unwrap();
        a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        let target = fixture.target(1, "follow-up");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = fixture.app.invoke_bound_target(
                &target,
                None,
                "fixture-user",
                true,
                false,
                None,
                None,
                None,
            );
        }));
        assert!(result.is_err());
        COMPOUND_MODE.set(0);
        assert!(a.undo_status(&mut fixture.app.state.doc).unwrap().can_redo);
        assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
        b.with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "later", true).unwrap();
        })
        .unwrap();
        b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Redo)
            .unwrap()
            .unwrap();
        fixture
            .records
            .read(&a.checkout.id, |doc| {
                assert!(doc.get(ROOT, "raw-unwind").unwrap().is_some());
                assert!(doc.get(ROOT, "typing").unwrap().is_some());
                assert!(doc.get(ROOT, "later").unwrap().is_none());
            })
            .unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "fresh and revoked authority share the full visible-row and activation oracle"
    )]
    fn task4_denied_bound_provider_rows_are_not_disclosed() {
        for initially_allowed in [false, true] {
            let mut fixture = ActionFixture::with_authority(false, true);
            let denied = crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
            if !initially_allowed {
                *fixture.permissions.borrow_mut() = denied.clone();
            }
            fixture.app.open_action_palette();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("sealed");
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
            if initially_allowed {
                assert!(
                    fixture
                        .app
                        .action_palette()
                        .unwrap()
                        .result_labels()
                        .contains(&"Hidden sealed write title".to_owned())
                );
                *fixture.permissions.borrow_mut() = denied;
                fixture.app.refresh_action_palette_resources();
                fixture
                    .app
                    .actions
                    .action_palette
                    .as_mut()
                    .unwrap()
                    .advance_search_page();
            }
            let labels = fixture.app.action_palette().unwrap().result_labels();
            assert!(!labels.contains(&"Hidden sealed write title".to_owned()));
            let allowed = labels
                .iter()
                .position(|label| label == "Allowed sealed sibling title")
                .unwrap();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_cursor(allowed);
            assert!(
                fixture
                    .app
                    .action_palette_handle_key(&action_palette_tests::synth_enter())
            );
            for (index, binding) in fixture.bindings.iter().enumerate() {
                fixture
                    .records
                    .read(&binding.checkout.id, |doc| {
                        assert_eq!(doc.get(ROOT, "follow-up").unwrap().is_some(), index == 1);
                    })
                    .unwrap();
            }
            fixture.app.open_action_palette();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("sealedopaque");
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
            let labels = fixture.app.action_palette().unwrap().result_labels();
            assert!(!labels.contains(&"Hidden sealedopaque title".to_owned()));
            assert!(labels.contains(&"Allowed sealedopaque title".to_owned()));
            assert!(
                fixture
                    .records
                    .read(&fixture.bindings[0].checkout.id, |doc| doc
                        .get(ROOT, "navigation")
                        .unwrap()
                        .is_none())
                    .unwrap()
            );
            assert!(
                fixture
                    .app
                    .state
                    .doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one controlled paging fixture covers completion, revocation and prompt pause"
    )]
    fn task4_completed_bound_search_validation_is_bounded() {
        let mut fixture = ActionFixture::with_authority(false, true);
        let confirmation = fixture.target(1, "follow-up");
        let counts = Rc::new([Cell::new(0), Cell::new(0)]);
        PAGED_SEARCH_COUNTS.with_borrow_mut(|slot| *slot = Some(counts.clone()));
        fixture.app.open_action_palette();
        PAGED_SEARCH_COUNTS.with_borrow_mut(|slot| *slot = None);
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("paged");
        let mut ticks = 0;
        while palette.advance_search_page().pending {
            ticks += 1;
            assert!(ticks < 20, "initial bounded query must finish");
        }
        assert_eq!(palette.result_labels().len(), 130, "no total-kind clipping");
        let step = |palette: &mut crate::action_palette::ActionPalette| {
            for counter in counts.iter() {
                counter.set(0);
            }
            let progress = palette.advance_search_page();
            let callbacks: usize = counts.iter().map(Cell::get).sum();
            assert!(
                callbacks <= 32,
                "{callbacks} resolver callbacks in one step"
            );
            assert!(progress.inspected <= 32);
            assert!(callbacks <= progress.inspected, "unaccounted provider work");
            progress
        };
        // A completed-query redraw starts at most one finite validation pass.
        let mut progress = step(palette);
        ticks = 0;
        while progress.pending {
            progress = step(palette);
            ticks += 1;
            assert!(
                ticks < 20,
                "completed validation must stop requesting redraws"
            );
        }
        assert_eq!(palette.result_labels().len(), 130);
        // Revoke during the next finite pass after this owner was visited;
        // queue order must not make the already-validated-row oracle accidental.
        ticks = 0;
        loop {
            assert!(step(palette).pending);
            if counts[0].get() > 0 {
                break;
            }
            ticks += 1;
            assert!(ticks < 5, "the first owner must participate in this pass");
        }
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
        ticks = 0;
        loop {
            progress = step(palette);
            assert!(
                palette
                    .result_labels()
                    .iter()
                    .all(|label| !label.starts_with("Revoked")),
                "unchecked stale rows must remain hidden"
            );
            if !progress.pending {
                break;
            }
            ticks += 1;
            assert!(ticks < 20, "revocation validation must finish");
        }
        assert_eq!(palette.result_labels().len(), 65);
        assert!(
            palette
                .result_labels()
                .iter()
                .all(|label| label.starts_with("Allowed"))
        );
        // The existing strict owner's follow-up enters the real confirmation
        // field. A partially drained validation pass must pause behind it.
        palette.set_query("");
        for tick in 0..20 {
            if !palette.advance_search_page().pending {
                break;
            }
            assert!(tick < 19, "reset query must complete");
        }
        assert!(step(palette).pending);
        assert!(palette.activate_bound(&confirmation));
        assert!(palette.in_argument_mode());
        assert!(palette.result_labels().is_empty());
        for _ in 0..2 {
            let progress = step(palette);
            assert_eq!(progress.inspected, 0);
            assert!(!progress.pending);
            assert!(palette.result_labels().is_empty());
        }
        palette.open();
        for tick in 0..20 {
            if !palette.advance_search_page().pending {
                break;
            }
            assert!(tick < 19, "reset query must complete");
        }
        assert_eq!(
            palette
                .result_labels()
                .iter()
                .filter(|label| label.starts_with("Allowed"))
                .count(),
            65
        );
    }

    #[test]
    fn task4_pending_status_selects_its_owner_without_closing_typing() {
        let mut fixture = ActionFixture::new();
        let binding = fixture.bindings[0].clone();
        let outer = fixture.app.state.doc.get_heads();
        binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "pending").unwrap();
            })
            .unwrap();
        let pending = fixture
            .records
            .read(
                &binding.checkout.id,
                automerge::transaction::Transactable::pending_ops,
            )
            .unwrap();
        assert!(
            binding
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_undo
        );
        assert!(
            !fixture.bindings[1]
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_undo
        );
        assert_eq!(
            fixture
                .records
                .read(
                    &binding.checkout.id,
                    automerge::transaction::Transactable::pending_ops
                )
                .unwrap(),
            pending
        );
        let entry = binding
            .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        assert_eq!(entry.instance_id, Some(11));
        assert!(
            binding
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_redo
        );
        assert!(
            fixture
                .records
                .read(&binding.checkout.id, |doc| doc
                    .get(ROOT, "typing")
                    .unwrap()
                    .is_none())
                .unwrap()
        );
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one controlled lifetime traces publication through refusal and delivery"
    )]
    fn task4_qualified_broker_request_retains_chosen_contract_and_directed_owner() {
        let mut fixture = ActionFixture::broker();
        let target = fixture.target(1, "follow-up");
        let resources = fixture.app.publication_resources();
        let actor = fixture.app.collaboration.local_author.0.clone();
        let evidence = serde_json::json!({"token": "controlled-confirmation"});
        let mut requests = Vec::new();
        // This seam captures submission only; no broker acceptance is simulated.
        fixture
            .app
            .invoke_bound_target_with_broker(
                &target,
                None,
                &actor,
                true,
                false,
                Some(&evidence),
                None,
                None,
                Some(&mut |request| requests.push(request)),
            )
            .unwrap();
        assert_eq!(requests.len(), 1);
        let CollabRequest::InvokePublishedAction {
            request_id,
            peer_id,
            resource,
            action,
            arguments,
            confirmation,
        } = requests.pop().unwrap()
        else {
            panic!("expected action request")
        };
        assert_eq!(peer_id, crate::collaboration::CURRENT_CONNECTION_PEER_ID);
        assert_eq!(request_id, 0);
        assert!(arguments.is_empty());
        assert_eq!(confirmation, evidence);
        let descriptor = tilo_actions::door::resolve(
            &resources,
            &tilo_actions::ActionAddress::new(&resource, &action),
        )
        .unwrap();
        assert_eq!(
            descriptor.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            fixture
                .app
                .actions
                .bound_publication
                .target(&tilo_actions::ActionAddress::new(&resource, &action))
                .unwrap()
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        for binding in &fixture.bindings {
            assert!(
                fixture
                    .records
                    .read(&binding.checkout.id, |doc| doc
                        .get(ROOT, "follow-up")
                        .unwrap()
                        .is_none())
                    .unwrap()
            );
        }
        fixture
            .app
            .invoke_published_action(41, resource, action, None, &actor, None);
        assert!(
            fixture
                .records
                .read_history(&fixture.bindings[0].checkout.id, |_, history| history
                    .next_undo_in(&fixture.bindings[0].author, 11)
                    .is_none())
                .unwrap()
        );
        assert!(
            fixture
                .records
                .read(&target.binding.checkout.id, |doc| doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .is_some())
                .unwrap()
        );
        assert_eq!(
            fixture
                .records
                .read_history(&target.binding.checkout.id, |_, history| history.revision())
                .unwrap(),
            1
        );
        let before = fixture.app.actions.next_broker_request_id;
        let mut sent = false;
        assert!(
            App::dispatch_broker_request(
                &mut fixture.app.actions.next_broker_request_id,
                false,
                &target.address.address,
                None,
                None,
                None,
                |_| sent = true
            )
            .is_err()
        );
        assert!(!sent);
        assert_eq!(fixture.app.actions.next_broker_request_id, before);
    }

    #[test]
    fn task4_checked_action_reuses_same_record_frame_and_refuses_cross_record() {
        for shared in [true, false] {
            let mut fixture = ActionFixture::with_same_record(shared);
            let target = fixture.target(1, "apply");
            let first = fixture.bindings[0].clone();
            let outer = fixture.app.state.doc.get_heads();
            let actor = fixture.app.collaboration.local_author.0.clone();
            let result = first
                .with_document(&mut fixture.app.state.doc, |doc, _| {
                    doc.put(ROOT, "before-nested", "A").unwrap();
                    let result = crate::bound_dispatch::invoke_resource_action(
                        fixture.app.root_widget.as_mut(),
                        doc,
                        &target,
                        fixture.app.actions.action_transport.as_mut(),
                        None,
                        None,
                        &actor,
                        true,
                        false,
                        tilo_actions::InvocationId::new(),
                        None,
                    );
                    doc.put(ROOT, "after-nested", "A").unwrap();
                    result
                })
                .unwrap();
            if shared {
                result.unwrap();
                first
                    .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
                fixture
                    .records
                    .read_history(&first.checkout.id, |doc, history| {
                        assert!(doc.get(ROOT, "after-nested").unwrap().is_none());
                        assert!(doc.get(ROOT, "before-nested").unwrap().is_some());
                        assert!(doc.get(ROOT, "follow-up").unwrap().is_some());
                        assert_eq!(
                            history.next_undo_in(&first.author, 12).unwrap().instance_id,
                            Some(12)
                        );
                    })
                    .unwrap();
            } else {
                assert!(matches!(
                    result,
                    Err(crate::bound_dispatch::BoundActionError::Dispatch(
                        crate::bound_dispatch::BoundDispatchError::CrossRecordReentry
                    ))
                ));
                assert!(
                    fixture
                        .records
                        .read(&target.binding.checkout.id, |doc| doc
                            .get(ROOT, "follow-up")
                            .unwrap()
                            .is_none())
                        .unwrap()
                );
            }
            assert_eq!(fixture.app.state.doc.get_heads(), outer);
        }
    }
    #[test]
    fn task4_lazy_palette_and_opaque_navigation_keep_provider_owner() {
        let mut fixture = ActionFixture::new();
        fixture.app.open_action_palette();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("lazy");
        for _ in 0..4 {
            palette.advance_search_page();
        }
        let targets = palette.referenced_bound_targets();
        assert_eq!(targets.len(), 2);
        fixture.app.refresh_action_palette_resources();
        let target = targets
            .iter()
            .find(|target| target.address.instance_id == 12)
            .unwrap();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        assert!(palette.activate_bound(target));
        let pending = palette.take_operation().unwrap();
        assert!(
            pending
                .bound
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        fixture
            .app
            .invoke_bound_target(target, None, "fixture-user", true, false, None, None, None)
            .unwrap();
        fixture.app.open_action_palette();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("opaque");
        for _ in 0..4 {
            palette.advance_search_page();
        }
        assert_eq!(palette.len(), 2);
        palette.set_cursor(1);
        palette.activate_selected();
        let (id, binding) = palette.take_scoped_navigation().unwrap();
        let binding = binding.unwrap();
        crate::bound_dispatch::activate_bound_search(
            fixture.app.root_widget.as_mut(),
            &mut fixture.app.state.doc,
            &binding,
            &id,
        )
        .unwrap();
        for owner in &fixture.bindings {
            assert_eq!(
                fixture
                    .records
                    .read(&owner.checkout.id, |doc| doc
                        .get(ROOT, "navigation")
                        .unwrap()
                        .is_some())
                    .unwrap(),
                owner.same_component(&binding)
            );
        }
        assert!(fixture.app.state.doc.get_heads().is_empty());
    }

    #[test]
    fn task4_reserved_publication_collision_never_routes_to_legacy_widget() {
        let mut fixture = ActionFixture::broker();
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        let old = std::mem::replace(
            &mut fixture.app.root_widget,
            Box::new(crate::widget::Text::new("empty")),
        );
        fixture.app.root_widget = Box::new(
            Column::new()
                .push(Box::new(Probe {
                    collision: Some(alias.clone()),
                    ..Probe::default()
                }))
                .push(old),
        );
        fixture
            .app
            .actions
            .action_registry
            .register::<FollowUp>("bound.apply");
        fixture.app.invalidate_widget_resource_cache();
        let resources = fixture.app.publication_resources();
        assert_eq!(
            resources
                .iter()
                .filter(|resource| resource.id.as_str() == alias)
                .count(),
            1
        );
        let descriptor = tilo_actions::door::resolve(
            &resources,
            &tilo_actions::ActionAddress::new(&alias, "follow-up"),
        )
        .unwrap();
        assert_eq!(
            descriptor.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            fixture
                .app
                .actions
                .bound_publication
                .alias(&fixture.bindings[0], &"x".repeat(4096))
                .is_err()
        );
    }
    fn assert_bound_broker_route_refused(
        fixture: &mut ActionFixture,
        target: &crate::bound_dispatch::BoundActionTarget,
        alias: &str,
        revision: &str,
        witness: Option<crate::collaboration::PrimaryRouteWitness>,
    ) {
        fixture.app.actions.bound_broker_route_for_test = witness;
        let resources = fixture.app.publication_resources();
        assert!(resources.iter().all(|resource| {
            !resource
                .id
                .as_str()
                .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
        }));
        assert!(
            fixture
                .app
                .resolve_publication_resource(alias, "follow-up")
                .is_none()
        );
        assert_ne!(fixture.app.action_directory_revision(), revision);
        let next_id = fixture.app.actions.next_broker_request_id;
        let mut sent = false;
        let error = fixture
            .app
            .invoke_bound_target_with_broker(
                target,
                None,
                "fixture-user",
                true,
                false,
                None,
                None,
                None,
                Some(&mut |_| sent = true),
            )
            .unwrap_err();
        assert_eq!(
            crate::bound_dispatch::door_error_code(&error),
            "bound_broker_route_unavailable"
        );
        assert!(!sent);
        assert_eq!(fixture.app.actions.next_broker_request_id, next_id);
        fixture.app.invoke_published_action(
            50,
            alias.to_owned(),
            "follow-up".into(),
            None,
            "fixture-user",
            None,
        );
        assert!(
            fixture
                .records
                .read(&target.binding.checkout.id, |doc| doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .is_none())
                .unwrap()
        );
        // Local component View actions do not need this manager's broker route.
        fixture
            .app
            .invoke_bound_target(
                &fixture.target(1, "view"),
                None,
                "fixture-user",
                true,
                false,
                None,
                None,
                None,
            )
            .unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one broker-route refusal test exercises every invalid witness against the same fixture"
    )]
    fn task4_bound_broker_route_refuses_wrong_app_workspace_record_and_missing_witness() {
        let mut fixture = ActionFixture::broker();
        let target = fixture.target(1, "follow-up");
        let matching = fixture
            .app
            .actions
            .bound_broker_route_for_test
            .clone()
            .unwrap();
        assert_eq!(fixture.app.publication_app_id(), matching.route.app_id);
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&target.binding, "bound:probe")
            .unwrap();
        let revision = fixture.app.action_directory_revision();
        let mut wrong_app = matching.clone();
        wrong_app.route.app_id = "so.tilo.other".into();
        let mut wrong_workspace = matching.clone();
        wrong_workspace.route.cluster_id = uuid::Uuid::nil();
        let mut wrong_record = matching.clone();
        wrong_record.route.record = Some("different-record".into());
        for witness in [
            Some(wrong_app),
            Some(wrong_workspace),
            Some(wrong_record),
            None,
        ] {
            assert_bound_broker_route_refused(&mut fixture, &target, &alias, &revision, witness);
        }
        let mut replacement = matching;
        replacement.epoch = uuid::Uuid::new_v4();
        fixture.app.actions.bound_broker_route_for_test = Some(replacement);
        assert_ne!(fixture.app.action_directory_revision(), revision);
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_some()
        );
    }

    #[test]
    fn task4_bound_control_authority_refreshes_without_an_outer_generation_change() {
        let mut fixture = ActionFixture::new();
        CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
        fixture.app.refresh_control_action_availability();
        assert_eq!(
            CONTROL_AUTHORITY.with_borrow(Clone::clone),
            vec![(false, true), (true, true)]
        );
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "fixture-user"),
                vec![],
            )
            .at_unix_time(100);
        CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
        fixture.app.refresh_control_action_availability();
        assert_eq!(
            CONTROL_AUTHORITY.with_borrow(Clone::clone),
            vec![(false, false), (true, false)]
        );
    }
}

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
mod prepared_runner_tests {
    use super::*;
    use crate::application::{AppComponent, LaunchTarget, RestoreInput};
    use crate::application_test_support::*;
    use crate::widget::TileHost;
    use automerge::ReadDoc;
    use winit::keyboard::{Key, NamedKey};

    fn editable_component(host: &ProbeHost, instance: u64) -> AppComponent {
        runner_probe_definition()
            .instantiate(
                host.prepare_with_permissions(
                    runner_probe_definition(),
                    instance,
                    &LaunchTarget::New,
                    runner_permissions(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap()
    }

    #[test]
    fn prepared_native_services_keep_explicit_presentation_and_watch_system_updates() {
        fn services(
            inputs: crate::application::ComponentRunnerInputs,
            binding: &crate::bound_dispatch::CallbackBinding,
        ) -> AppConstructionInputs {
            let mut services = AppConstructionInputs::controlled();
            services.debug_mode = || true;
            let mut presence = PresenceCtx::empty();
            presence.set_own_identity(["announced-handle"]);
            services.presence_ctx = ConstructionValue::Ready(presence);
            AppConstructionInputs::component_with_services(inputs, binding, services)
        }
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let mut app =
            App::from_component_using(component, presentation_inputs(2.0), services).unwrap();
        assert_eq!(app.local_author(), &author());
        assert!(app.presence_ctx.is_own_identity(&author().0));
        assert!(app.presence_ctx.is_own_identity("announced-handle"));
        assert!(app.debug_mode);
        assert!(app.watch_system_settings);
        assert_presentation(&app.theme, 1.5, 1.25);
        let due = app.last_theme_store_check + Duration::from_millis(750);
        assert!(app.reconcile_configured_theme_with(
            due,
            || Some(std::time::SystemTime::UNIX_EPOCH),
            |app| {
                app.accessibility.base_theme = Theme::light();
            },
        ));
        assert_eq!(app.theme.name, Theme::light().name);
        assert_eq!(app.theme.text_scale, 1.5);
        assert_eq!(app.theme.ui_scale, 1.25);
        assert!(app.theme.reduce_motion);
        draw_prepared_component_test_frame(&mut app);
        app.close_components();
        assert_eq!(app.component_close_reports().len(), 1);
        assert!(app.component_close_reports()[0].record_error.is_none());
    }

    #[test]
    fn prepared_native_due_theme_poll_preserves_explicit_inputs_without_discovery() {
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let mut app = App::from_component(component, presentation_inputs(2.0)).unwrap();
        let observed = observe_presentation();
        let checks = std::cell::Cell::new(0);
        let applies = std::cell::Cell::new(0);
        let due = app.last_theme_store_check + Duration::from_millis(750);
        let changed = app.reconcile_configured_theme_with(
            due,
            || {
                checks.set(checks.get() + 1);
                Some(std::time::SystemTime::UNIX_EPOCH)
            },
            |app| {
                applies.set(applies.get() + 1);
                app.accessibility.base_theme = Theme::light();
                // This controlled stand-in never creates a narrator or reads settings.
                app.accessibility.a11y.screen_reader = true;
            },
        );
        draw_prepared_component_test_frame(&mut app);
        assert_eq!(
            checks.get(),
            0,
            "prepared polling must not consult its source"
        );
        assert_eq!(applies.get(), 0);
        assert!(!changed);
        assert!(app.theme_store_stamp.is_none());
        assert!(app.accessibility.screen_reader.is_none());
        assert!(!app.accessibility.a11y.screen_reader);
        assert_presentation(&app.accessibility.base_theme, 2.0, 1.25);
        let observation = observed.borrow();
        let (theme, renderer_scale) = observation.draws.last().unwrap();
        assert_eq!(*renderer_scale, 1.5);
        assert_presentation(theme, 1.5, 1.25);
        app.close_components();
    }

    #[test]
    fn legacy_theme_poll_keeps_due_stamp_and_reload_behavior_with_controlled_sources() {
        let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
            Box::new(crate::widget::Column::new())
        });
        let start = app.last_theme_store_check;
        let checks = std::cell::Cell::new(0);
        let applies = std::cell::Cell::new(0);
        let stamp = Some(std::time::SystemTime::UNIX_EPOCH);
        for (millis, source_stamp, expected_checks, expected_applies, expected_changed) in [
            (749, stamp, 0, 0, false),
            (750, stamp, 1, 1, true),
            (1500, stamp, 2, 1, false),
            (2250, None, 3, 2, true),
        ] {
            app.layout.layout_dirty = false;
            app.layout.structure_dirty = false;
            let changed = app.reconcile_configured_theme_with(
                start + Duration::from_millis(millis),
                || {
                    checks.set(checks.get() + 1);
                    source_stamp
                },
                |app| {
                    applies.set(applies.get() + 1);
                    app.accessibility.base_theme = Theme::light();
                },
            );
            assert_eq!(changed, expected_changed);
            assert_eq!(checks.get(), expected_checks);
            assert_eq!(applies.get(), expected_applies);
            assert_eq!(app.layout.layout_dirty, expected_changed);
            assert_eq!(app.layout.structure_dirty, expected_changed);
            assert!(app.accessibility.screen_reader.is_none());
        }
        assert!(app.theme_store_stamp.is_none());
        assert_eq!(app.theme.name, Theme::light().name);
    }

    fn wait_for_saved_record(host: &ProbeHost, binding: &crate::bound_dispatch::CallbackBinding) {
        assert!(host.records.same_owner(&binding.records));
        let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
        loop {
            let status = host.records.save_status(&binding.checkout.id).unwrap();
            if status == crate::record::RecordSaveStatus::Saved {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "original writer did not acknowledge: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_saved_body(
        host: &ProbeHost,
        binding: &crate::bound_dispatch::CallbackBinding,
        expected: &str,
    ) {
        wait_for_saved_record(host, binding);
        let path = app_state::DocStore::room_record_dir_at(
            &host.root(),
            APP,
            ROOM,
            &binding.checkout.id.storage_key(),
        )
        .unwrap()
        .join(app_state::DOCUMENT_FILE);
        let doc = automerge::AutoCommit::load_with_options(
            &std::fs::read(path).unwrap(),
            automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error),
        )
        .unwrap();
        assert_eq!(
            doc.get(automerge::ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some(expected)
        );
    }

    fn edit_bound_record(app: &mut App, binding: &crate::bound_dispatch::CallbackBinding) {
        let target = crate::bound_dispatch::BoundActionTarget {
            binding: binding.clone(),
            address: crate::bound_dispatch::BoundActionAddress {
                instance_id: binding.instance_id,
                address: tilo_actions::ActionAddress::new("probe:shared", "edit"),
            },
        };
        app.invoke_bound_target(
            &target,
            None,
            "probe-author",
            false,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(body(&binding.records, &binding.checkout), "edited");
        assert_eq!(
            binding.records.save_status(&binding.checkout.id).unwrap(),
            crate::record::RecordSaveStatus::Saving
        );
    }

    #[test]
    fn prepared_native_autosave_tick_acknowledges_original_writer_before_close() {
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let binding = component.callback_binding();
        let mut app = App::from_component(component, runner_inputs()).unwrap();
        assert!(app.persistence.autosave.is_none() && app.persistence.records.is_none());
        assert_saved_body(&host, &binding, "initialized");
        edit_bound_record(&mut app, &binding);
        app.autosave_tick().unwrap();
        assert_saved_body(&host, &binding, "edited");
        assert!(binding.lifetime.is_active());
        assert!(app.component_close_reports().is_empty());
        app.close_components();
    }

    #[test]
    fn prepared_native_autosave_tick_reaches_distinct_embedded_owners_before_close() {
        let first_host = ProbeHost::new();
        let first = editable_component(&first_host, 11);
        let first_binding = first.callback_binding();
        let mut app = App::from_component(first, runner_inputs()).unwrap();
        let second_host = ProbeHost::new();
        let mut second = editable_component(&second_host, 22);
        let second_binding = second.callback_binding();
        assert!(!first_binding.records.same_owner(&second_binding.records));
        let handle = app.wake_scope.as_ref().unwrap().handle();
        second.mount(move |_| handle.wake()).unwrap();
        let first_root =
            std::mem::replace(&mut app.root_widget, Box::new(crate::widget::Column::new()));
        app.root_widget = Box::new(
            crate::widget::Column::new()
                .push(first_root)
                .push(Box::new(TileHost::from_component(second))),
        );
        assert_saved_body(&first_host, &first_binding, "initialized");
        assert_saved_body(&second_host, &second_binding, "initialized");
        edit_bound_record(&mut app, &first_binding);
        edit_bound_record(&mut app, &second_binding);
        app.autosave_tick().unwrap();
        assert_saved_body(&first_host, &first_binding, "edited");
        assert_saved_body(&second_host, &second_binding, "edited");
        assert!(first_binding.lifetime.is_active() && second_binding.lifetime.is_active());
        assert!(app.component_close_reports().is_empty());
        app.close_components();
    }

    enum Runner {
        App(Box<App>),
        Driver(Box<crate::headless::Driver>),
    }
    impl Runner {
        fn new(mode: u8, mut component: AppComponent) -> Self {
            match mode {
                0 => Self::App(Box::new(
                    App::from_component(component, runner_inputs()).unwrap(),
                )),
                1 => {
                    let scope = crate::WakeScope::new();
                    let handle = scope.handle();
                    component.mount(move |_| handle.wake()).unwrap();
                    let author = component.callback_binding().author;
                    Self::App(Box::new(App::from_scoped_parts_with_inputs(
                        scope,
                        State::new(),
                        Box::new(TileHost::from_component(component)),
                        || {
                            let mut inputs = AppConstructionInputs::controlled();
                            let mut presence = PresenceCtx::empty();
                            presence.set_own_identity([author.0.clone()]);
                            inputs.local_author = ConstructionValue::Ready(author);
                            inputs.presence_ctx = ConstructionValue::Ready(presence);
                            inputs
                        },
                    )))
                }
                _ => Self::Driver(Box::new(
                    crate::headless::Driver::from_component(component, (320, 180), runner_inputs())
                        .unwrap(),
                )),
            }
        }
        fn invoke(&mut self, action: &str) -> Result<(), String> {
            match self {
                Self::App(app) => {
                    app.init_layout();
                    app.refresh_semantic_tree();
                    let id = app
                        .semantic_tree()
                        .iter()
                        .find(|node| node.name.as_deref() == Some("Shared probe"))
                        .ok_or_else(|| "resource absent after authority projection".to_owned())?
                        .id
                        .clone();
                    if app.invoke_semantic_action(&id, action, None)? {
                        Ok(())
                    } else {
                        Err("action absent".into())
                    }
                }
                Self::Driver(driver) => driver
                    .invoke_bound_action(
                        &crate::bound_dispatch::BoundActionAddress {
                            instance_id: 11,
                            address: tilo_actions::ActionAddress::new("probe:shared", action),
                        },
                        &serde_json::Map::new(),
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
            }
        }
        fn close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
            match self {
                Self::App(app) => {
                    app.close_components();
                    app.take_component_close_reports()
                }
                Self::Driver(driver) => {
                    driver.close_components();
                    driver.take_component_close_reports()
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one source record and View are compared across three real runner paths"
    )]
    fn prepared_three_runner_factory_record_view_action_and_denial_parity() {
        let seed = ProbeHost::new();
        let mut component = runner_probe_definition()
            .instantiate(
                seed.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        component
            .restore_local_view(&crate::view::View {
                scroll: vec![37.0],
                focus: Some(crate::view::FocusView {
                    ordinal: 0,
                    ephemeral: "caret=9".into(),
                }),
                ..crate::view::View::default()
            })
            .unwrap();
        let saved = component.close().recovery.unwrap();
        let bytes = seed
            .records
            .with(&checkout.id, automerge::AutoCommit::save)
            .unwrap();
        let mut results = Vec::new();
        for mode in 0..3 {
            let host = ProbeHost::new();
            let path = app_state::DocStore::room_record_dir_at(
                &host.root(),
                APP,
                ROOM,
                &checkout.id.storage_key(),
            )
            .unwrap()
            .join(app_state::DOCUMENT_FILE);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let target = existing(&checkout);
            let permissions = runner_permissions();
            let context = host
                .prepare_with_permissions(
                    runner_probe_definition(),
                    11,
                    &target,
                    permissions.clone(),
                )
                .unwrap();
            let component = runner_probe_definition()
                .instantiate(context, target, RestoreInput::Local(saved.clone()))
                .unwrap();
            assert_eq!(calls(), 1);
            assert_eq!(initializations(), 0);
            assert_eq!(body(&host.records, &checkout), "initialized");
            assert_eq!(component.capture_local_view().unwrap(), saved.view);
            host.records
                .with(&checkout.id, |doc| {
                    use automerge::transaction::Transactable;
                    assert_eq!(doc.pending_ops(), 0);
                    // A fresh CRDT actor sorts before the copied historical actors.
                    // It is independent of the authenticated participant identity.
                    doc.set_actor(automerge::ActorId::from([0_u8].as_slice()));
                    assert_eq!(doc.pending_ops(), 0);
                })
                .unwrap();
            let mut runner = Runner::new(mode, component);
            runner.invoke("view").unwrap();
            runner.invoke("effect").unwrap();
            assert_eq!(
                *operation_observations()[0].borrow(),
                vec!["View", "Effect"]
            );
            runner.invoke("edit").unwrap();
            assert_eq!(body(&host.records, &checkout), "edited");
            host.records
                .read_history(&checkout.id, |_, history| {
                    assert!(history.next_undo_in(&author(), 11).is_some());
                })
                .unwrap();
            *permissions.borrow_mut() = crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "probe-author"),
                vec![],
            )
            .at_unix_time(100);
            assert!(runner.invoke("edit").is_err());
            assert_eq!(body(&host.records, &checkout), "edited");
            assert_eq!(
                *operation_observations()[0].borrow(),
                vec!["View", "Effect"]
            );
            let reports = runner.close();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].recovery.as_ref().unwrap().view, saved.view);
            assert!(reports[0].view_error.is_none());
            results.push(
                serde_json::to_vec(
                    &host
                        .records
                        .read(&checkout.id, |doc| {
                            doc.get(automerge::ROOT, "body")
                                .unwrap()
                                .unwrap()
                                .0
                                .to_str()
                                .unwrap()
                                .to_owned()
                        })
                        .unwrap(),
                )
                .unwrap(),
            );
            host.records
                .with_history(&checkout.id, |doc, history| {
                    assert!(history.undo_in(doc, &author(), 11, None, None).is_some());
                    assert!(history.next_undo_in(&author(), 11).is_none());
                })
                .unwrap();
            assert_eq!(body(&host.records, &checkout), "initialized");
        }
        assert_eq!(results, vec![b"\"edited\"".to_vec(); 3]);
    }

    #[test]
    fn prepared_three_runner_close_retains_original_view_store_for_retry() {
        for mode in 0..3 {
            let host = ProbeHost::new();
            let component = runner_probe_definition()
                .instantiate(
                    host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                        .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let path = host.view_path(11);
            std::fs::create_dir_all(&path).unwrap();
            let mut runner = Runner::new(mode, component);
            drop(host.views);
            let mut reports = runner.close();
            assert_eq!(reports.len(), 1);
            let report = &mut reports[0];
            assert!(report.view_error.is_some());
            assert!(report.view_retry.is_some());
            assert_eq!(report.recovery.as_ref().unwrap().app_id, APP);
            drop(runner);
            std::fs::remove_dir(&path).unwrap();
            report.retry_view().unwrap();
            assert!(report.view_error.is_none());
            assert!(report.view_retry.is_none());
            let saved: crate::application_view::SavedInstanceView =
                postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved.instance_id, 11);
            assert_eq!(saved.device_id, DEVICE);
            assert_eq!(saved.workspace_id, ROOM);
        }
    }
    #[test]
    fn prepared_native_close_retains_original_recovery_and_is_idempotent() {
        let host = ProbeHost::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let binding = component.callback_binding();
        let path = host.view_path(11);
        std::fs::create_dir_all(&path).unwrap();
        let mut app = App::from_component(component, runner_inputs()).unwrap();
        drop(host.views);
        app.close_wake_scope();
        let _closed = app.enter_wake_scope();
        app.close_native_content();
        assert!(!binding.lifetime.is_active());
        assert_eq!(app.component_close_reports().len(), 1);
        assert!(app.component_close_reports()[0].view_error.is_some());
        app.close_native_content();
        let mut reports = app.take_component_close_reports();
        assert_eq!(reports.len(), 1);
        assert!(binding.records.read(&binding.checkout.id, |_| ()).is_some());
        assert_eq!(reports[0].recovery.as_ref().unwrap().instance_id, 11);
        std::fs::remove_dir(&path).unwrap();
        reports[0].retry_view().unwrap();
        assert!(path.is_file());
        assert!(app.take_component_close_reports().is_empty());
    }

    #[test]
    fn prepared_three_runner_close_cancels_local_operations_and_rejects_stale_results() {
        for mode in 0..3 {
            let host = ProbeHost::new();
            let component = runner_probe_definition()
                .instantiate(
                    host.prepare_with_permissions(
                        runner_probe_definition(),
                        11,
                        &LaunchTarget::New,
                        runner_permissions(),
                    )
                    .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let checkout = component.record().clone();
            let writer = local_state().writer();
            let observed = operation_observations()[0].clone();
            let mut runner = Runner::new(mode, component);
            runner.invoke("pending").unwrap();
            assert_eq!(*observed.borrow(), vec!["pending"]);
            let reports = runner.close();
            assert_eq!(*observed.borrow(), vec!["pending", "cancelled"]);
            assert!(!writer.try_set("stale after close".into()));
            let target = existing(&checkout);
            let successor = runner_probe_definition()
                .instantiate(
                    host.prepare_with_permissions(
                        runner_probe_definition(),
                        11,
                        &target,
                        runner_permissions(),
                    )
                    .unwrap(),
                    target,
                    RestoreInput::Local(reports[0].recovery.clone().unwrap()),
                )
                .unwrap();
            let successor_local = local_state();
            let mut successor = Runner::new(mode, successor);
            assert!(!writer.try_set("stale after reopen".into()));
            assert_eq!(successor_local.with(Clone::clone), "construction wake");
            successor.invoke("view").unwrap();
            successor.close();
            assert_eq!(body(&host.records, &checkout), "initialized");
            assert_eq!(*observed.borrow(), vec!["pending", "cancelled"]);
        }
    }

    fn palette_key(app: &mut App, key: Key, text: Option<&str>) {
        let mut event = action_palette_tests::synth_enter();
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.logical_key = key;
            event.text = text.map(Into::into);
        }
        assert!(app.action_palette_handle_key(&event));
    }

    #[test]
    fn prepared_presentation_replaces_effective_factors_across_runners_and_setters() {
        for theme_scale in [0.5, 1.0, 1.5, 2.0] {
            for native in [true, false] {
                let host = ProbeHost::new();
                let component = runner_probe_definition()
                    .instantiate(
                        host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                            .unwrap(),
                        LaunchTarget::New,
                        RestoreInput::None,
                    )
                    .unwrap();
                let inputs = presentation_inputs(theme_scale);
                let observed = observe_presentation();
                if native {
                    let mut app = App::from_component(component, inputs).unwrap();
                    for scale in [1.5, 0.5, 4.0, 4.0, 0.75] {
                        app.set_text_scale(scale);
                        draw_prepared_component_test_frame(&mut app);
                        let observation = observed.borrow();
                        let (theme, renderer_scale) = observation.draws.last().unwrap();
                        assert_eq!(*renderer_scale, scale);
                        assert_presentation(theme, scale, 1.25);
                        assert_presentation(observation.layouts.last().unwrap(), scale, 1.25);
                    }
                    app.set_ui_scale(1.5);
                    app.set_ui_scale(1.5);
                    app.set_high_contrast(true);
                    assert_eq!(app.theme.name, "High Contrast");
                    assert_eq!(app.theme.text_scale, 0.75);
                    assert_eq!(app.theme.ui_scale, 1.5);
                    app.set_high_contrast(false);
                    draw_prepared_component_test_frame(&mut app);
                    let observation = observed.borrow();
                    let (theme, renderer_scale) = observation.draws.last().unwrap();
                    assert_eq!(*renderer_scale, 0.75);
                    assert_presentation(theme, 0.75, 1.5);
                    assert!(!observation.motion.is_empty());
                    assert!(observation.motion.iter().all(|value| *value));
                    app.close_components();
                } else {
                    let mut driver =
                        crate::headless::Driver::from_component(component, (800, 600), inputs)
                            .unwrap();
                    let observation = observed.borrow();
                    let (theme, renderer_scale) = observation.draws.last().unwrap();
                    assert_eq!(*renderer_scale, 1.5);
                    assert_presentation(theme, 1.5, 1.25);
                    assert_presentation(observation.layouts.last().unwrap(), 1.5, 1.25);
                    assert!(!observation.motion.is_empty());
                    assert!(observation.motion.iter().all(|value| *value));
                    driver.close_components();
                }
            }
        }
    }
    fn palette_frame(app: &mut App) {
        let mut scene = Scene::new();
        draw_frame_content(app, &mut scene, 800, 600, 1.0, None);
        let palette = app.action_palette().unwrap();
        assert!(palette.rect().width() > 0.0 && palette.rect().height() > 0.0);
        assert_eq!(
            crate::action_palette::observed_field_language(palette),
            "de-DE"
        );
        assert_eq!(app.text_renderer.text_scale(), 1.5);
    }
    fn activate_palette_action(app: &mut App, action: &str) {
        app.refresh_semantic_tree();
        let target = app
            .accessibility
            .semantic_tree
            .bound_actions_for_test()
            .into_iter()
            .find(|(_, target)| {
                target.address.instance_id == 11 && target.address.address.action == action
            })
            .unwrap()
            .1;
        assert!(
            app.actions
                .action_palette
                .as_mut()
                .unwrap()
                .activate_bound(&target)
        );
    }
    #[test]
    fn component_palette_uses_host_text_through_prompt_confirmation_and_reopen() {
        let host = ProbeHost::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare_with_permissions(
                    runner_probe_definition(),
                    11,
                    &LaunchTarget::New,
                    runner_permissions(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        let catalog = crate::font_catalog::FontCatalog::controlled_fixture(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
            "de-DE",
        );
        let mut renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
        renderer.set_text_scale(1.5);
        renderer.bump_generation();
        let mut app = App::from_component(
            component,
            crate::application::ComponentRunnerInputs {
                theme: Theme::dark(),
                text_renderer: renderer,
            },
        )
        .unwrap();
        assert_eq!(app.local_author(), &author());
        app.init_layout();
        app.open_action_palette();
        let before = app.text_renderer.cache_stats();
        palette_frame(&mut app);
        assert!(
            app.text_renderer.cache_stats().1 > before.1,
            "measurement uses the host cache"
        );
        palette_key(&mut app, Key::Character("P".into()), Some("P"));
        assert_eq!(app.action_palette().unwrap().query(), "P");
        palette_frame(&mut app);
        activate_palette_action(&mut app, "prompt");
        assert!(app.action_palette().unwrap().in_argument_mode());
        palette_frame(&mut app);
        palette_key(&mut app, Key::Named(NamedKey::Escape), None);
        assert!(!app.action_palette().unwrap().in_argument_mode());
        palette_frame(&mut app);
        activate_palette_action(&mut app, "prompt");
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("prompted");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "prompted");
        app.open_action_palette();
        palette_frame(&mut app);
        activate_palette_action(&mut app, "typed");
        palette_frame(&mut app);
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query(r#"{"value":"typed","enabled":true}"#);
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "typed");
        app.open_action_palette();
        activate_palette_action(&mut app, "confirm");
        palette_frame(&mut app);
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("wrong");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "typed");
        app.actions.action_palette.as_mut().unwrap().set_query("JA");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "confirmed");
        app.open_action_palette();
        let before = app.text_renderer.cache_stats();
        app.text_renderer.bump_generation();
        palette_frame(&mut app);
        assert!(app.text_renderer.cache_stats().1 > before.1);
        host.records
            .read_history(&checkout.id, |_, history| {
                assert_eq!(
                    history.next_undo_in(&author(), 11).unwrap().author.as_ref(),
                    Some(&author())
                );
            })
            .unwrap();
        app.close_components();
    }
}

#[cfg(test)]
mod card_grid_host_tests {
    use super::*;
    use crate::widget::{Button, Card, CardGrid, Column, Container, Element};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn fixture(count: usize) -> (App, Rc<RefCell<Vec<usize>>>) {
        let clicks = Rc::new(RefCell::new(Vec::new()));
        let cards: Vec<Element> = (0..count)
            .map(|i| {
                let clicks = clicks.clone();
                Box::new(Card::new(Box::new(
                    Button::new(&format!("card-{i}"), &format!("Card {i}"), None)
                        .on_click(move || clicks.borrow_mut().push(i)),
                ))) as Element
            })
            .collect();
        let root = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(20.0)
            .push(Box::new(
                CardGrid::new(cards)
                    .with_min_card_width(260.0)
                    .with_card_height(180.0)
                    .with_gap(16.0),
            ))
            .push(Box::new(
                Container::new().with_no_padding().with_height(10.0),
            ));
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        (app, clicks)
    }

    fn layout(app: &mut App, width: u32) {
        app.layout.layout_dirty = true;
        run_layout_pass(
            app,
            app.layout.root_node.unwrap(),
            width,
            2000,
            1.0,
            false,
            None,
        )
        .unwrap();
    }

    fn button_rects(app: &App) -> Vec<KurboRect> {
        app.root_widget.children()[0]
            .children()
            .iter()
            .map(|card| card.children()[0].children()[0].get_rect())
            .collect()
    }

    #[test]
    fn card_grid_buttons_have_host_focus_and_geometry_before_paint() {
        let (mut app, clicks) = fixture(6);
        layout(&mut app, 900);
        assert_eq!(
            app.focusable_order.len(),
            6,
            "all card buttons enter host focus traversal"
        );
        let before = button_rects(&app);
        for (index, &node) in app.focusable_order.iter().enumerate() {
            assert_eq!(
                app.inspect_node(node, |widget| widget.name().to_owned())
                    .unwrap(),
                "Button"
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_rect()).unwrap(),
                before[index]
            );
            assert!(before[index].width() > 0.0 && before[index].height() > 0.0);
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().semantic_key)
                    .unwrap(),
                Some(format!("card-{index}"))
            );
        }
        let target = before[4].center();
        assert!(app.dispatch_semantic_click(target.x, target.y));
        assert_eq!(*clicks.borrow(), vec![4]);
        assert_eq!(app.focused_node, Some(app.focusable_order[4]));
        let order = app.focusable_order.clone();
        let mut scene = Scene::new();
        app.root_widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut app.text_renderer,
            &app.theme,
            &PresenceCtx::default(),
        );
        assert_eq!(
            button_rects(&app),
            before,
            "paint must not establish or change geometry"
        );
        assert_eq!(app.focusable_order, order);
    }

    #[test]
    fn card_grid_reflows_retained_nodes_and_keeps_explicit_rows() {
        let (mut app, _) = fixture(6);
        layout(&mut app, 900);
        let order = app.focusable_order.clone();
        app.set_focused_node(Some(order[4]));
        for (width, height, second_row_y) in [
            (900, 376.0, 196.0),
            (600, 572.0, 196.0),
            (200, 1160.0, 196.0),
            (900, 376.0, 196.0),
        ] {
            layout(&mut app, width);
            let grid = app.root_widget.children()[0];
            assert_eq!(grid.get_rect().height(), height);
            assert_eq!(grid.get_rect().width(), f64::from(width));
            let cards = grid.children();
            let columns = match width {
                900 => 3,
                600 => 2,
                _ => 1,
            };
            assert_eq!(cards[columns].get_rect().y0, second_row_y);
            for card in cards {
                assert_eq!(card.get_rect().height(), 180.0);
                assert!(card.get_rect().x1 <= f64::from(width));
            }
            assert_eq!(app.root_widget.children()[1].get_rect().y0, height + 20.0);
            assert_eq!(
                app.focusable_order, order,
                "resize retains focus identities"
            );
            assert_eq!(app.focused_node, Some(order[4]));
        }
    }

    #[test]
    fn card_grid_long_content_does_not_turn_fixed_rows_into_minimums() {
        let body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(crate::widget::Text::new(
                &"Long content wraps across lines. ".repeat(80),
            )))
            .push(Box::new(Button::new("long-action", "Action", None)));
        let root = CardGrid::new(vec![Box::new(Card::new(Box::new(body)))])
            .with_card_height(180.0)
            .with_min_card_width(260.0);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        layout(&mut app, 200);
        assert_eq!(app.root_widget.get_rect().height(), 180.0);
        assert_eq!(app.root_widget.children()[0].get_rect().height(), 180.0);
    }

    #[test]
    fn card_grid_headless_uses_same_narrow_geometry_and_click_target() {
        let (mut app, clicks) = fixture(2);
        let root = std::mem::replace(&mut app.root_widget, Box::new(Column::new()));
        let mut driver = crate::headless::Driver::new_with_controlled_inputs(
            State::new(),
            root,
            (200, 600),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("grid-test".into()),
        );
        let snapshot = driver.inspection();
        let grid = &snapshot.root.children[0];
        assert_eq!(grid.rect, [0.0, 0.0, 200.0, 376.0]);
        assert_eq!(grid.children[1].rect, [0.0, 196.0, 200.0, 180.0]);
        driver.click_named("card-1").unwrap();
        assert_eq!(*clicks.borrow(), vec![1]);
    }

    #[test]
    fn card_grid_under_scrollable_clips_input_before_paint() {
        let (mut app, clicks) = fixture(6);
        let child = std::mem::replace(&mut app.root_widget, Box::new(Column::new()));
        app.root_widget = Box::new(crate::widget::Scrollable::new_vertical().with_child(child));
        app.init_layout();
        app.layout.layout_dirty = true;
        let root_node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root_node, 200, 190, 1.0, true, None).unwrap();
        let root = app.root_widget.children()[0];
        assert_eq!(root.children()[0].get_rect().height(), 1160.0);
        let hidden_button = root.children()[0].children()[1].children()[0].children()[0].get_rect();
        assert!(hidden_button.y0 >= 190.0);
        let target = hidden_button.center();
        app.dispatch_semantic_click(target.x, target.y);
        assert!(
            clicks.borrow().is_empty(),
            "off-viewport card must not receive pointer activation"
        );
        let visible_button =
            app.root_widget.children()[0].children()[0].children()[0].children()[0].children()[0]
                .get_rect()
                .center();
        app.dispatch_semantic_click(visible_button.x, visible_button.y);
        assert_eq!(*clicks.borrow(), vec![0]);
    }

    #[test]
    fn card_grid_retains_local_edit_state_across_width_changes() {
        let input = crate::widget::TextInput::local().with_initial_value("saved value");
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CardGrid::new(vec![Box::new(Card::new(Box::new(input)))])),
        );
        app.init_layout();
        layout(&mut app, 900);
        let node = app.focusable_order[0];
        app.set_focused_node(Some(node));
        let state = app.inspect_node(node, |widget| widget.get_state()).unwrap();
        assert!(state.is_some());
        for width in [200, 600, 900] {
            layout(&mut app, width);
            assert_eq!(app.focusable_order, vec![node]);
            assert_eq!(app.focused_node, Some(node));
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_state()).unwrap(),
                state
            );
        }
    }

    #[test]
    fn card_grid_forwards_retained_list_structure_changes_to_host() {
        use crate::widget::{ListModel, VirtualList};
        use std::cell::Cell;
        struct GrowingModel {
            keys: Rc<RefCell<Vec<u64>>>,
            changed: Rc<Cell<bool>>,
        }
        impl ListModel for GrowingModel {
            type Key = u64;
            fn len(&self) -> usize {
                self.keys.borrow().len()
            }
            fn key_at(&self, index: usize) -> Option<u64> {
                self.keys.borrow().get(index).copied()
            }
            fn index_of(&self, key: &u64) -> Option<usize> {
                self.keys
                    .borrow()
                    .iter()
                    .position(|candidate| candidate == key)
            }
            fn poll_changed(&mut self) -> bool {
                self.changed.replace(false)
            }
        }
        let keys = Rc::new(RefCell::new(vec![1, 2]));
        let changed = Rc::new(Cell::new(false));
        let list = VirtualList::new(
            GrowingModel {
                keys: keys.clone(),
                changed: changed.clone(),
            },
            32.0,
            |_, _, key| Box::new(Button::new(&format!("row-{key}"), "Row", None)),
        )
        .with_viewport_height(120.0);
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CardGrid::new(vec![Box::new(Card::new(Box::new(list)))])),
        );
        app.init_layout();
        layout(&mut app, 300);
        let button_keys = |app: &App| {
            app.focusable_order
                .iter()
                .filter_map(|&node| {
                    app.inspect_node(node, |widget| {
                        if widget.name() == "Button" {
                            widget
                                .accessibility()
                                .and_then(|access| access.semantic_key)
                        } else {
                            None
                        }
                    })
                    .unwrap()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(button_keys(&app), vec!["row-1", "row-2"]);
        app.root_widget.take_structure_dirty();
        keys.borrow_mut().push(3);
        changed.set(true);
        assert!(app.root_widget.update(&mut app.state.doc, &app.state.root));
        app.root_widget.sync(&mut app.state.doc, &app.state.root);
        assert!(app.root_widget.take_structure_dirty());
        app.rebuild_structure();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 300, 2000, 1.0, true, None).unwrap();
        assert_eq!(button_keys(&app), vec!["row-1", "row-2", "row-3"]);
        // Rebuilding and post-build sync must preserve actual delegate geometry.
        for node in &app.focusable_order {
            assert!(app.layout.taffy.layout(*node).unwrap().size.height > 0.0);
            assert!(
                app.inspect_node(*node, |widget| widget.get_rect().height())
                    .unwrap()
                    > 0.0
            );
        }
    }

    #[test]
    fn card_grid_nested_below_minimum_uses_actual_parent_allocation() {
        let nested = CardGrid::new(vec![
            Box::new(Button::new("nested-a", "A", None)),
            Box::new(Button::new("nested-b", "B", None)),
        ])
        .with_min_card_width(300.0)
        .with_card_height(60.0)
        .with_gap(16.0);
        let outer = CardGrid::new(vec![Box::new(Card::new(Box::new(nested)))])
            .with_min_card_width(260.0)
            .with_card_height(180.0);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(outer));
        app.init_layout();
        layout(&mut app, 200);
        let card = app.root_widget.children()[0];
        let surface = card.children()[0];
        let nested = surface.children()[0];
        assert_eq!(card.get_rect().width(), 200.0);
        assert!(nested.get_rect().width() < 200.0);
        let buttons = nested.children();
        assert_eq!(buttons[1].get_rect().y0 - buttons[0].get_rect().y0, 76.0);
        assert!(
            buttons
                .iter()
                .all(|button| button.get_rect().x1 <= card.get_rect().x1)
        );
        assert_eq!(app.focusable_order.len(), 2);
    }

    struct GrowingBody {
        column: Column,
        requested: Rc<Cell<usize>>,
        count: usize,
        dirty: bool,
        clicks: Rc<RefCell<Vec<usize>>>,
    }
    impl Widget for GrowingBody {
        fn update(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) -> bool {
            if self.count == self.requested.get() {
                return false;
            }
            self.count = self.requested.get();
            self.column = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(8.0);
            for index in 0..self.count {
                let clicks = self.clicks.clone();
                self.column = std::mem::take(&mut self.column).push(Box::new(
                    Button::new(&format!("dynamic-{index}"), "Action", None)
                        .on_click(move || clicks.borrow_mut().push(index)),
                ));
            }
            self.dirty = true;
            true
        }
        fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
            self.column.sync(state, path);
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            theme: &Theme,
            map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.column.build_layout(tree, theme, map)
        }
        fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.column.apply_layout(tree, node, origin);
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.column.draw(scene, transform, text, theme, presence);
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            scale: f64,
            modifiers: &winit::event::Modifiers,
            state: &mut automerge::AutoCommit,
            path: &automerge::ObjId,
            handled: &mut bool,
            focus: &mut Option<NodeId>,
            node: NodeId,
            tree: &TaffyTree<()>,
        ) {
            self.column.handle_event(
                event, scale, modifiers, state, path, handled, focus, node, tree,
            );
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.column.children()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.column.children_mut()
        }
        fn get_rect(&self) -> KurboRect {
            self.column.get_rect()
        }
        fn name(&self) -> &'static str {
            "GrowingBody"
        }
        fn take_structure_dirty(&mut self) -> bool {
            std::mem::take(&mut self.dirty)
        }
    }

    #[test]
    fn card_grid_rebuilt_card_body_has_prepaint_geometry_and_input() {
        let requested = Rc::new(Cell::new(1));
        let clicks = Rc::new(RefCell::new(Vec::new()));
        let body = GrowingBody {
            column: Column::new(),
            requested: requested.clone(),
            count: 0,
            dirty: false,
            clicks: clicks.clone(),
        };
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(
                CardGrid::new(vec![Box::new(Card::new(Box::new(body)))]).with_card_height(180.0),
            ),
        );
        app.root_widget.update(&mut app.state.doc, &app.state.root);
        app.init_layout();
        layout(&mut app, 300);
        assert_eq!(app.focusable_order.len(), 1);
        app.root_widget.take_structure_dirty();
        requested.set(2);
        assert!(app.root_widget.update(&mut app.state.doc, &app.state.root));
        assert!(app.root_widget.take_structure_dirty());
        app.rebuild_structure();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 300, 2000, 1.0, true, None).unwrap();
        assert_eq!(app.focusable_order.len(), 2);
        let rects = app
            .focusable_order
            .iter()
            .map(|&node| app.inspect_node(node, |widget| widget.get_rect()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rects[0].height(), 40.0);
        assert_eq!(rects[1].height(), 40.0);
        assert_eq!(rects[1].y0 - rects[0].y1, 8.0);
        let target = rects[1].center();
        assert!(app.dispatch_semantic_click(target.x, target.y));
        assert_eq!(*clicks.borrow(), vec![1]);
        let mut scene = Scene::new();
        app.root_widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut app.text_renderer,
            &app.theme,
            &PresenceCtx::default(),
        );
        for (&node, before) in app.focusable_order.iter().zip(rects) {
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_rect()).unwrap(),
                before
            );
        }
    }

    #[test]
    fn card_grid_empty_and_single_keep_intrinsic_height() {
        for (count, height) in [(0, 0.0), (1, 180.0)] {
            let (mut app, _) = fixture(count);
            layout(&mut app, 200);
            assert_eq!(app.root_widget.children()[0].get_rect().height(), height);
            assert_eq!(app.focusable_order.len(), count);
        }
    }
}

#[cfg(test)]
mod viewport_composition_tests {
    use super::*;
    use crate::widget::TaffyTreeExt;
    use crate::widget::{Button, Element};
    use std::cell::Cell;
    use std::rc::Rc;

    /// Legacy composition deliberately does not report its breakpoint changes.
    struct ResponsiveRoot {
        rect: KurboRect,
        children: Vec<Element>,
        nodes: Vec<NodeId>,
        builds: Rc<Cell<usize>>,
        editing: bool,
        trap: bool,
        modal_on_narrow: bool,
        native_calls: Option<Rc<std::cell::RefCell<Vec<NativeCall>>>>,
        scroll: Rc<Cell<(f32, f32)>>,
        oscillate: Rc<Cell<bool>>,
        paints: Rc<Cell<usize>>,
    }
    impl ResponsiveRoot {
        fn new(builds: Rc<Cell<usize>>) -> Self {
            Self {
                rect: KurboRect::ZERO,
                children: vec![],
                nodes: vec![],
                builds,
                editing: false,
                trap: false,
                modal_on_narrow: false,
                native_calls: None,
                scroll: Rc::new(Cell::new((0.0, 120.0))),
                oscillate: Rc::default(),
                paints: Rc::default(),
            }
        }
    }
    impl Widget for ResponsiveRoot {
        fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
            for child in &mut self.children {
                child.sync(doc, path);
            }
        }
        fn name(&self) -> &'static str {
            "ResponsiveRoot"
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            scale: f64,
            modifiers: &winit::event::Modifiers,
            doc: &mut automerge::AutoCommit,
            path: &automerge::ObjId,
            handled: &mut bool,
            focus: &mut Option<NodeId>,
            _: NodeId,
            taffy: &TaffyTree<()>,
        ) {
            for (child, node) in self.children.iter_mut().zip(&self.nodes) {
                child.handle_event(
                    event, scale, modifiers, doc, path, handled, focus, *node, taffy,
                );
            }
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.builds.set(self.builds.get().saturating_add(1));
            let count = if self.rect.width() >= 1100.0 {
                3
            } else if self.rect.width() >= 800.0 {
                2
            } else {
                1
            };
            self.children = (0..count)
                .map(|index| {
                    Box::new(Button::new(&format!("action-{index}"), "Action", None)) as Element
                })
                .collect();
            if self.editing {
                self.children = vec![
                    Box::new(
                        crate::widget::TextInput::new(vec![crate::presence::PathSegment::Field(
                            "body".into(),
                        )])
                        .with_on_change_path("body")
                        .with_id("editor"),
                    ),
                    Box::new(Button::new("other", "Other", None)),
                ];
                if self.rect.width() < 1100.0 {
                    self.children.reverse();
                }
                self.children.push(Box::new(
                    crate::widget::Scrollable::new_vertical()
                        .with_height(100.0)
                        .with_persisted_offset(self.scroll.clone())
                        .with_child(Box::new(
                            crate::widget::Container::new()
                                .with_no_padding()
                                .with_height(1000.0),
                        )),
                ));
            }
            if self.modal_on_narrow {
                // This fixture opens a modal whose fallback is another editor,
                // so accidental transfer of the outside selection is observable.
                if self.rect.width() < 1100.0 {
                    self.children.swap(0, 1);
                }
                let mut body = crate::widget::Column::new();
                for child in self.children.drain(..2) {
                    body = body.push(child);
                }
                let mut modal = crate::widget::ModalDialog::new("Modal", body);
                modal.set_open(self.rect.width() < 1100.0);
                self.children = vec![
                    Box::new(
                        crate::widget::TextInput::new(vec![])
                            .with_on_change_path("body")
                            .with_id("outside"),
                    ),
                    Box::new(modal),
                ];
            }
            if let Some(calls) = &self.native_calls {
                self.children = vec![Box::new(crate::widget::NativeView::new(
                    "Native",
                    CountingNativeBackend {
                        generation: self.builds.get(),
                        calls: calls.clone(),
                    },
                ))];
            }
            self.nodes = self
                .children
                .iter_mut()
                .map(|child| child.build_layout(taffy, theme, map))
                .collect();
            let node = taffy.parent_node(
                taffy::Style {
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::length(if self.editing {
                            400.0
                        } else if self.oscillate.get() && self.rect.height() == 100.0 {
                            101.0
                        } else {
                            100.0
                        }),
                    },
                    ..Default::default()
                },
                &self.nodes,
            );
            map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(
            &mut self,
            taffy: &TaffyTree<()>,
            node: NodeId,
            origin: vello::kurbo::Point,
        ) {
            let layout = taffy.layout(node).unwrap();
            self.rect = KurboRect::from_origin_size(
                origin,
                (f64::from(layout.size.width), f64::from(layout.size.height)),
            );
            for (child, node) in self.children.iter_mut().zip(&self.nodes) {
                child.apply_layout(taffy, *node, origin);
            }
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            affine: Affine,
            text: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.paints.set(self.paints.get().saturating_add(1));
            for child in &mut self.children {
                child.draw(scene, affine, text, theme, presence);
            }
        }
        fn get_rect(&self) -> KurboRect {
            self.rect
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.children.iter().map(AsRef::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
            self.children
                .iter_mut()
                .map(|child| child.as_mut() as &mut dyn Widget)
                .collect()
        }
        fn traps_focus(&self) -> bool {
            self.trap
        }
        fn layout_needs_measure(&self) -> bool {
            false
        }
    }
    fn frame(app: &mut App, width: u32) {
        app.layout.layout_dirty = true;
        run_layout_pass(
            app,
            app.layout.root_node.unwrap(),
            width,
            400,
            1.0,
            false,
            None,
        )
        .unwrap();
    }
    #[test]
    fn legacy_composition_matches_fresh_at_multiple_viewports_before_paint() {
        let builds = Rc::new(Cell::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(builds.clone())),
        );
        app.init_layout();
        for (width, count) in [(1200, 3), (920, 2), (600, 1), (1200, 3), (920, 2)] {
            frame(&mut app, width);
            assert_eq!(app.root_widget.children().len(), count, "viewport {width}");
            let mut fresh = App::new_with_controlled_inputs(
                State::new(),
                Box::new(ResponsiveRoot::new(Rc::new(Cell::new(0)))),
            );
            fresh.init_layout();
            frame(&mut fresh, width);
            assert_eq!(
                app.root_widget
                    .children()
                    .iter()
                    .map(|child| child.get_rect())
                    .collect::<Vec<_>>(),
                fresh
                    .root_widget
                    .children()
                    .iter()
                    .map(|child| child.get_rect())
                    .collect::<Vec<_>>()
            );
        }
        let before = builds.get();
        frame(&mut app, 920);
        assert_eq!(
            builds.get(),
            before,
            "translation-only frames do not rebuild composition"
        );
        run_layout_pass(&mut app, NodeId::from(0usize), 920, 400, 1.0, false, None).unwrap();
        assert_eq!(
            builds.get(),
            before,
            "paint-only frames do not rebuild composition"
        );
    }
    #[test]
    fn logical_viewport_changes_recompose_even_at_unchanged_physical_size() {
        let builds = Rc::new(Cell::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(builds.clone())),
        );
        app.init_layout();
        frame(&mut app, 1200);
        let node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, node, 1200, 400, 2.0, false, None).unwrap();
        assert_eq!(app.root_widget.children().len(), 1);
        let before = builds.get();
        let node = app.layout.root_node.unwrap();
        app.layout.layout_dirty = true;
        run_layout_pass(&mut app, node, 600, 200, 1.0, false, None).unwrap();
        assert_eq!(
            builds.get(),
            before,
            "equal logical viewport does not recompose"
        );
    }

    #[test]
    fn pending_structure_rebuild_is_measured_at_an_unchanged_viewport() {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(Rc::default())),
        );
        app.init_layout();
        frame(&mut app, 1200);
        app.layout.structure_dirty = true;
        frame(&mut app, 1200);
        assert_eq!(app.root_widget.get_rect().width(), 1200.0);
        assert!(
            app.root_widget
                .children()
                .iter()
                .all(|child| child.get_rect().height() > 0.0)
        );
    }

    fn editing_fixture() -> (State, ResponsiveRoot) {
        use automerge::transaction::Transactable;
        let mut state = State::new();
        let body = state
            .doc
            .put_object(&automerge::ROOT, "body", automerge::ObjType::Text)
            .unwrap();
        state.doc.splice_text(&body, 0, 0, "hello world").unwrap();
        let mut root = ResponsiveRoot::new(Rc::default());
        root.editing = true;
        (state, root)
    }

    #[test]
    fn resize_restores_fresh_editor_by_identity_with_selection_and_scroll() {
        let (state, root) = editing_fixture();
        let scroll = root.scroll.clone();
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        app.set_focused_node(Some(app.focusable_order[0]));
        app.mutate_node(app.focused_node.unwrap(), |widget| {
            widget.set_state("5:b:1");
        })
        .unwrap();
        for width in [920, 600, 1200] {
            frame(&mut app, width);
            let node = app.focused_node.expect("editor remains focused");
            let expected_index = usize::from(width < 1100);
            assert_eq!(
                node, app.focusable_order[expected_index],
                "focus follows identity across reordered controls"
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().semantic_key)
                    .unwrap()
                    .as_deref(),
                Some("editor")
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_state())
                    .unwrap()
                    .as_deref(),
                Some("5:b:1")
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().value)
                    .unwrap()
                    .as_deref(),
                Some("hello world")
            );
            assert_eq!(scroll.get(), (0.0, 120.0));
            assert!(
                app.inspect_node(node, |widget| widget.get_rect().width())
                    .unwrap()
                    > 0.0
            );
        }
    }

    fn harness(state: State, root: ResponsiveRoot, width: u32) -> crate::headless::FrameHarness {
        crate::headless::FrameHarness::new_with_controlled_inputs(
            state,
            Box::new(root),
            (width, 400),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("resize-test".into()),
        )
    }

    #[test]
    fn headless_resize_matches_fresh_and_preserves_selected_edit_target() {
        let builds = Rc::new(Cell::new(0));
        let mut driver = harness(State::new(), ResponsiveRoot::new(builds.clone()), 1200);
        for width in [920, 600, 1200] {
            driver.resize((width, 400)).unwrap();
            let fresh = harness(State::new(), ResponsiveRoot::new(Rc::default()), width);
            assert_eq!(driver.inspection().root.rect, fresh.inspection().root.rect);
            assert_eq!(
                driver
                    .inspection()
                    .root
                    .children
                    .iter()
                    .map(|child| child.rect)
                    .collect::<Vec<_>>(),
                fresh
                    .inspection()
                    .root
                    .children
                    .iter()
                    .map(|child| child.rect)
                    .collect::<Vec<_>>()
            );
        }
        let before = builds.get();
        driver.resize((1200, 400)).unwrap();
        assert_eq!(builds.get(), before);

        let (state, root) = editing_fixture();
        let scroll = root.scroll.clone();
        let mut editor = harness(state, root, 1200);
        editor.tab(false).expect("the editor is the first tab stop");
        editor.set_modifiers(&["ctrl".into()]);
        editor.key("a");
        editor.set_modifiers(&[]);
        editor.resize((920, 400)).unwrap();
        assert_eq!(
            editor
                .focused_access_node()
                .unwrap()
                .semantic_key
                .as_deref(),
            Some("editor")
        );
        assert_eq!(scroll.get(), (0.0, 120.0));
        editor.type_text("X");
        assert_eq!(
            editor.focused_access_node().unwrap().value.as_deref(),
            Some("X"),
            "typing replaces the selection retained across fresh composition"
        );
    }

    #[test]
    fn oscillating_composition_is_bounded_and_headless_skips_paint() {
        let root = ResponsiveRoot::new(Rc::default());
        let oscillate = root.oscillate.clone();
        let paints = root.paints.clone();
        let builds = root.builds.clone();
        let mut driver = harness(State::new(), root, 1200);
        oscillate.set(true);
        let before = builds.get();
        let painted = paints.get();
        assert!(matches!(
            driver.resize((920, 400)),
            Err(crate::widget::WidgetLayoutError::CompositionDidNotSettle)
        ));
        assert!(builds.get() - before <= 8);
        assert_eq!(
            paints.get(),
            painted,
            "failed settlement never reaches paint"
        );
        oscillate.set(false);
        driver.resize((920, 400)).unwrap();
        assert_eq!(paints.get(), painted + 1);

        let root = ResponsiveRoot::new(Rc::default());
        root.oscillate.set(true);
        let builds = root.builds.clone();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        let root = app.layout.root_node.unwrap();
        assert!(run_layout_pass(&mut app, root, 1200, 400, 1.0, false, None).is_none());
        assert!(builds.get() <= 8);
        assert!(app.layout.last_layout_logical.is_none());
    }
    #[derive(Clone, Debug, PartialEq)]
    enum NativeCall {
        Mount(usize),
        Unmount(usize),
        Geometry(usize, KurboRect),
        Visible(usize, bool),
    }
    struct CountingNativeBackend {
        generation: usize,
        calls: Rc<std::cell::RefCell<Vec<NativeCall>>>,
    }
    impl crate::widget::NativeViewBackend for CountingNativeBackend {
        fn mount(&mut self, _: &crate::widget::NativeViewHost<'_>) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(NativeCall::Mount(self.generation));
            Ok(())
        }
        fn unmount(&mut self) {
            self.calls
                .borrow_mut()
                .push(NativeCall::Unmount(self.generation));
        }
        fn set_geometry(&mut self, geometry: crate::widget::NativeViewGeometry) {
            self.calls
                .borrow_mut()
                .push(NativeCall::Geometry(self.generation, geometry.logical));
        }
        fn set_visible(&mut self, visible: bool) {
            self.calls
                .borrow_mut()
                .push(NativeCall::Visible(self.generation, visible));
        }
        fn set_focus(&mut self, _: bool) {}
        fn set_enabled(&mut self, _: bool) {}
    }

    #[test]
    fn review_failed_headless_resize_can_return_to_last_successful_size() {
        let root = ResponsiveRoot::new(Rc::default());
        let oscillate = root.oscillate.clone();
        let mut driver = harness(State::new(), root, 1200);
        oscillate.set(true);
        assert!(driver.resize((920, 400)).is_err());
        oscillate.set(false);
        driver.resize((1200, 400)).unwrap();
        assert_eq!(driver.inspection().viewport, [1200, 400]);
        assert_eq!(driver.inspection().root.children.len(), 3);
        assert_eq!(driver.inspection().root.rect[2], 1200.0);
        driver.resize((920, 400)).unwrap();
        assert_eq!(driver.inspection().root.children.len(), 2);
    }

    #[test]
    fn review_active_trap_retains_keyed_selection_after_replacing_and_reordering_controls() {
        let (state, mut root) = editing_fixture();
        root.trap = true;
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        let editor = app.focusable_order[0];
        app.set_focused_node(Some(editor));
        app.mutate_node(editor, |widget| widget.set_state("5:b:1"))
            .unwrap();
        for width in [920, 600, 1200] {
            frame(&mut app, width);
            assert!(app.accessibility.focus_trap_active);
            let focused = app.focused_node.expect("modal editor remains focused");
            assert!(app.focusable_order.contains(&focused));
            assert_eq!(
                app.inspect_node(focused, |widget| widget
                    .accessibility()
                    .unwrap()
                    .semantic_key)
                    .unwrap()
                    .as_deref(),
                Some("editor")
            );
            assert!(
                app.inspect_node(focused, |widget| widget.is_focused())
                    .unwrap()
            );
            assert_eq!(
                app.inspect_node(focused, |widget| widget.get_state())
                    .unwrap()
                    .as_deref(),
                Some("5:b:1")
            );
        }
    }

    #[test]
    fn review_native_views_mount_only_after_successful_final_composition() {
        let mut root = ResponsiveRoot::new(Rc::default());
        let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
        root.native_calls = Some(calls.clone());
        let builds = root.builds.clone();
        let oscillate = root.oscillate.clone();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        let host = crate::widget::NativeViewHost::headless(app.input.platform, 1.0);
        crate::widget::mount_native_views(app.root_widget.as_mut(), &host);
        for width in [1200, 920] {
            let mounts = calls
                .borrow()
                .iter()
                .filter(|call| matches!(call, NativeCall::Mount(_)))
                .count();
            app.layout.layout_dirty = true;
            let node = app.layout.root_node.unwrap();
            run_layout_pass(&mut app, node, width, 400, 1.0, false, Some(&host)).unwrap();
            let final_generation = builds.get();
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .filter(|call| matches!(call, NativeCall::Mount(_)))
                    .count(),
                mounts + 1,
                "mount final composition exactly once"
            );
            assert!(
                calls
                    .borrow()
                    .contains(&NativeCall::Mount(final_generation))
            );
            assert!(calls.borrow().iter().any(|call| matches!(call, NativeCall::Geometry(generation, rect) if *generation == final_generation && rect.width() == f64::from(width))));
            let mut scene = Scene::new();
            app.root_widget.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut app.text_renderer,
                &app.theme,
                &PresenceCtx::default(),
            );
            assert!(
                calls
                    .borrow()
                    .contains(&NativeCall::Visible(final_generation, true))
            );
        }
        let mounts = calls
            .borrow()
            .iter()
            .filter(|call| matches!(call, NativeCall::Mount(_)))
            .count();
        oscillate.set(true);
        app.layout.layout_dirty = true;
        let node = app.layout.root_node.unwrap();
        assert!(run_layout_pass(&mut app, node, 600, 400, 1.0, false, Some(&host)).is_none());
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|call| matches!(call, NativeCall::Mount(_)))
                .count(),
            mounts,
            "failed intermediate compositions never attach"
        );
        oscillate.set(false);
        let node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, node, 920, 400, 1.0, false, Some(&host)).unwrap();
        assert_eq!(
            app.root_widget.get_rect().width(),
            920.0,
            "live recovery also invalidates failed geometry"
        );
    }
    #[test]
    fn review_new_modal_does_not_restore_outside_focus_or_copy_selection_to_fallback() {
        let (state, mut root) = editing_fixture();
        root.modal_on_narrow = true;
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        assert!(!app.accessibility.focus_trap_active);
        let outside = app.focusable_order[0];
        app.set_focused_node(Some(outside));
        app.mutate_node(outside, |widget| widget.set_state("5:b:1"))
            .unwrap();
        frame(&mut app, 920);
        assert!(app.accessibility.focus_trap_active);
        let focused = app.focused_node.expect("modal owns focus");
        assert!(app.focusable_order.contains(&focused));
        assert!(
            app.inspect_node(focused, |widget| widget.is_focused())
                .unwrap()
        );
        assert_ne!(
            app.inspect_node(focused, |widget| widget
                .accessibility()
                .unwrap()
                .semantic_key)
                .unwrap()
                .as_deref(),
            Some("outside")
        );
        assert_eq!(
            app.inspect_node(focused, |widget| widget.get_state())
                .unwrap()
                .as_deref(),
            Some("0"),
            "fallback editor keeps its own caret and no selection"
        );
        let outside = app
            .layout
            .node_map
            .keys()
            .copied()
            .find(|&node| {
                app.inspect_node(node, |widget| {
                    widget
                        .accessibility()
                        .and_then(|access| access.semantic_key)
                        .as_deref()
                        == Some("outside")
                })
                .unwrap_or(false)
            })
            .unwrap();
        assert!(
            !app.inspect_node(outside, |widget| widget.is_focused())
                .unwrap()
        );
    }
}

#[cfg(test)]
#[path = "card_composition_tests.rs"]
mod card_composition_tests;

#[cfg(test)]
#[path = "form_composition_tests.rs"]
mod form_composition_tests;

#[cfg(test)]
#[path = "notes_rich_controls_tests.rs"]
mod notes_rich_controls_tests;
