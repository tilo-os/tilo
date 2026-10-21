//! A framework app shown as a window of its own, the way every app runs on a
//! device: the shell sees only the pixels of its surface and what it says on
//! its shell-ui connection.
//!
//! The app runs here, in the shots process, but behind the same boundary a
//! separate process has: it is laid out and drawn by a headless frame of its
//! own, its pixels reach the shell as a foreign surface's frame, and the
//! window bar reaches it only over a socket carrying the real protocol's
//! frames (its slots and what its window offers one way, the bar's events the
//! other). Nothing of it is in the shell's widget tree.

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::UnixStream;

use shell::shell::overlay_host::ShellOverlayHost;
use ui::floating_surface::{
    FloatingSurfaceClientMessage, FloatingSurfaceHostMessage, FloatingSurfaceServiceClient,
};

/// One app in a window of its own.
pub struct OwnProcessApp {
    /// The app id the launcher vouched for on its connection.
    pub app_id: &'static str,
    /// The shell-ui connection the shell knows it by.
    pub connection: u64,
    /// The surface its window shows.
    pub surface_id: u64,
    frame: ui::headless::FrameHarness,
    socket: UnixStream,
    pending: Vec<u8>,
    size: (u32, u32),
    /// Renders the window's frames, at its device size.
    renderer: Option<ui::offscreen::OffscreenRenderer>,
    ground: ui::vello::peniko::Color,
}

impl OwnProcessApp {
    /// Run `app` as the window of `surface_id`, at `size`, in `theme`.
    ///
    /// # Errors
    /// The socket between the app and the shell could not be set up.
    pub fn new(
        app: ui::App,
        app_id: &'static str,
        connection: u64,
        surface_id: u64,
        size: (u32, u32),
        theme: ui::theme::Theme,
    ) -> Result<Self, String> {
        let (app_side, shell_side) = UnixStream::pair().map_err(|error| error.to_string())?;
        shell_side
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let ground = theme.tokens.colors.surfaces.content;
        let app = ui::app::EmbeddedApp::new(app, |_| {}).map_err(|error| error.to_string())?;
        let mut frame = ui::headless::FrameHarness::for_root(ui::State::new(), app, size, theme);
        let client =
            FloatingSurfaceServiceClient::from_test_stream(app_side).map_err(|e| e.to_string())?;
        app_runtime(&mut frame).test_floating_service_connection(client);
        Ok(Self {
            app_id,
            connection,
            surface_id,
            frame,
            socket: shell_side,
            pending: Vec::new(),
            size,
            renderer: None,
            ground,
        })
    }

    /// One turn of the app's loop against the shell: hand it what the bar
    /// sent, let it answer, carry its messages to the shell and present its
    /// window's frame.
    pub fn serve(
        &mut self,
        host: &mut ShellOverlayHost,
        events: Vec<ui::window_toolbar::ToolbarEvent>,
    ) {
        for event in events {
            let mut wire = serde_json::to_vec(&FloatingSurfaceHostMessage::ToolbarEvent { event })
                .unwrap_or_default();
            wire.push(b'\n');
            if let Err(error) = self.socket.write_all(&wire) {
                eprintln!("shots: the app's connection broke: {error}");
            }
        }
        // Twice: a message the app reads on one turn is answered on the next.
        for _ in 0..2 {
            self.frame
                .root_mut::<ui::app::EmbeddedApp>()
                .pump_shell_connection_for_test();
            for message in self.read() {
                host.receive_window_bar_message(self.connection, self.app_id, &message);
            }
        }
        self.present();
    }

    /// Lay the app out at its window's size and publish its frame as the
    /// surface's.
    fn present(&mut self) {
        if let Some((w, h)) = shell::shell::foreign::store().viewport(self.surface_id)
            && w > 0
            && h > 0
            && (w, h) != self.size
        {
            self.size = (w, h);
            self.renderer = None;
            if let Err(error) = self.frame.resize((w, h)) {
                eprintln!("shots: the app's window did not lay out: {error}");
            }
        }
        self.frame.relayout();
        // The frame renders at the capture's device scale, as a client
        // draws its buffer at its output's scale.
        let scale = ui::offscreen::capture_device_scale().max(1);
        let (w, h) = (
            self.size.0.saturating_mul(scale),
            self.size.1.saturating_mul(scale),
        );
        let renderer = self
            .renderer
            .get_or_insert_with(|| ui::offscreen::OffscreenRenderer::new(w, h));
        let rgba = self.frame.render_rgba_with(renderer, self.ground);
        shell::shell::foreign::store().publish_rgba(self.surface_id, &rgba, w, h);
    }

    /// What the app sent since the last call, frame by frame.
    fn read(&mut self) -> Vec<FloatingSurfaceClientMessage> {
        let mut chunk = [0_u8; 8192];
        loop {
            match self.socket.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => self
                    .pending
                    .extend_from_slice(chunk.get(..n).unwrap_or_default()),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => {
                    eprintln!("shots: the app's connection broke: {error}");
                    break;
                }
            }
        }
        let mut messages = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let Some(body) = line.get(..line.len().saturating_sub(1)) else {
                continue;
            };
            if body.is_empty() {
                continue;
            }
            match serde_json::from_slice(body) {
                Ok(message) => messages.push(message),
                Err(error) => {
                    eprintln!("shots: the app sent a frame the shell cannot read: {error}");
                }
            }
        }
        messages
    }
}

/// The app's own runtime inside the frame that draws it.
fn app_runtime(frame: &mut ui::headless::FrameHarness) -> &mut ui::App {
    frame.root_mut::<ui::app::EmbeddedApp>().app_mut()
}
