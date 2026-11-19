//! A mount's GPU and what it tells the page about the frame it draws: letting
//! go of the device a failed frame drew on and making another, showing the
//! frame it already has instead of drawing an empty one, the colours a page
//! showing one app draws its own bar in, and the caret and place in the
//! workspace its live layer shows. Each method is `Mount`'s.
//!
//! A device can be lost for reasons the page never caused (a driver reset,
//! memory pressure, a browser that took the device for another tab), and a
//! device that errors cannot be trusted to draw again either. Neither is a
//! reason for the desktop to stop: the mount makes a new device, surface and
//! renderer and redraws, keeping every document it holds. A GPU that keeps
//! failing is not chased for ever: past a few remakes in a short while
//! ([`crate::gpu_recovery::GpuRemakes`]) the desktop stops and says why, as
//! it did before.

use wasm_bindgen::JsValue;
use winit::event::WindowEvent;

use super::{Gpu, Mount, unconfigure};

impl Mount {
    /// Present the frame the target already holds on `output`, drawing
    /// nothing: a texture once acquired shows whatever it holds, so a frame
    /// skipped after acquiring it would otherwise blank the canvas. A
    /// target that no longer holds the last frame (a resize made it anew)
    /// has nothing to show again.
    pub(super) fn keep_last_frame(&mut self, output: wgpu::SurfaceTexture) -> Result<(), String> {
        let gpu = self.gpu.as_mut().ok_or("WebGPU renderer missing")?;
        if gpu.retained.frames.shows_frame() {
            let shared = gpu.shared.try_borrow().map_err(|_| "shared GPU busy")?;
            let handle = shared
                .cx
                .devices
                .get(gpu.surface.dev_id)
                .ok_or("WebGPU device missing")?;
            ui::gpu::submit_target_to_surface(
                &handle.device,
                &handle.queue,
                &gpu.surface,
                &output.texture,
            );
        }
        output.present();
        // The viewport this frame applied is what input maps onto now.
        self.viewport.did_present();
        Ok(())
    }

    /// The published caret or selection as live selection anchors
    /// (`{anchor, head}`, hex), for the page's live layer: the keyboard
    /// focus's when it is in the focused window (or this mount's
    /// application), else that window's editor caret
    /// ([`tilo_desktop_surface::published_selection`]).
    fn focus_presence_value(&self) -> Option<serde_json::Value> {
        use ui::collaboration::PresencePayload;
        let keyboard = self.frame.focus_presence();
        let (key, payload) = match &self.desktop {
            Some(desktop) => desktop.handle.published_selection(keyboard),
            None => tilo_desktop_surface::published_selection(
                keyboard,
                &ui::presence::capability::subtree_presence(self.frame.root_widget()),
            ),
        }?;
        let (start, end) = match payload {
            PresencePayload::Caret { pos } => (pos, pos),
            PresencePayload::Selection { start, end } => (start, end),
            _ => return None,
        };
        let (anchor, head) = tilo_server_protocol::live::presence_anchors(&key, start, end)?;
        Some(
            serde_json::json!({"anchor": hex::encode(anchor.cursor), "head": hex::encode(head.cursor)}),
        )
    }

    /// Tell a page showing one application the colours it is drawn in, when
    /// they changed: the page's own bar over it takes them, so the two read
    /// as one surface in whatever theme the person chose. `pageTheme`:
    /// `{dark, ground, ink, quiet, line}`, each colour `#rrggbb`. A desktop
    /// draws its own bar and is never told.
    pub(super) fn add_page_theme(&self, value: &mut serde_json::Value) {
        let generation = crate::page_theme::generation();
        if self.desktop.is_some() || self.theme_told.get() == Some(generation) {
            return;
        }
        let Some(object) = value.as_object_mut() else {
            return;
        };
        self.theme_told.set(Some(generation));
        let theme = crate::page_theme::current();
        let hex = |color: ui::peniko::Color| {
            let rgba = color.to_rgba8();
            format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b)
        };
        object.insert(
            "pageTheme".into(),
            serde_json::json!({
                "dark": theme.document.is_dark(),
                "ground": hex(theme.tokens.colors.surfaces.content),
                "ink": hex(theme.text_primary),
                "quiet": hex(theme.text_dim),
                "line": hex(theme.border_standard),
            }),
        );
    }

    /// Add the focus presence and the place in the workspace to a status where
    /// they differ from what the page was last told.
    pub(super) fn add_presence_changes(&self, value: &mut serde_json::Value) {
        let Some(object) = value.as_object_mut() else {
            return;
        };
        let presence = self.focus_presence_value();
        if *self.focus_presence.borrow() != presence {
            object.insert(
                "focusPresence".into(),
                presence.clone().unwrap_or(serde_json::Value::Null),
            );
            *self.focus_presence.borrow_mut() = presence;
        }
        let place = self.live_place_value();
        if *self.live_place.borrow() != place {
            object.insert(
                "livePlace".into(),
                place.clone().unwrap_or(serde_json::Value::Null),
            );
            *self.live_place.borrow_mut() = place;
        }
    }

    /// A frame failed with `error`. When the GPU is what failed (its device
    /// lost, or an error from it), the device, surface and renderer are made
    /// again and the shell redraws on them, keeping every document; anything
    /// else, or a GPU that keeps failing ([`crate::gpu_recovery`]), stops the
    /// desktop with `error`.
    pub(super) fn frame_failed(&mut self, id: u64, error: String) {
        let gpu_failed = self.gpu.as_ref().and_then(Gpu::failure).is_some();
        if gpu_failed && self.gpu_remakes.admit(ui::time::now()) {
            let lived = self
                .gpu
                .as_ref()
                .and_then(|gpu| gpu.shared.try_borrow().ok().map(|shared| shared.made))
                .map_or(ui::time::Duration::ZERO, |made| {
                    ui::time::now().saturating_duration_since(made)
                });
            let other = crate::gpu_recovery::device_lost(lived);
            let on = if other {
                "on the other GPU"
            } else {
                "on the same GPU"
            };
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "Browser runtime: the GPU failed ({error}); making it again {on} and drawing on"
            )));
            self.remake_gpu(id);
            return;
        }
        if gpu_failed {
            // The canvas would keep showing the last frame the lost device
            // presented, which may be half drawn: the page says the desktop
            // stopped over an empty one instead.
            unconfigure(&self.canvas);
        }
        self.error = Some(error);
        self.publish(id);
    }

    /// A pressed key reaching the shell: how many frames were presented by then.
    pub(super) fn note_key(&mut self, event: &WindowEvent) {
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            self.frames_at_key = Some(self.frames);
        }
    }
}
