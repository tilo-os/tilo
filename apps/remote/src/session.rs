//! Session surfaces — the live area you see after connecting.
//!
//! There are two fundamentally different surfaces:
//!
//! * **Terminal** (SSH): a scrollback grid of styled cells rendered with
//!   the toolkit's monospace text path and ANSI palette. This module
//!   holds the cell model and a small ANSI-ish sample transcript so the
//!   session reads as a real shell when offline.
//!
//! * **Framebuffer** (VNC/RDP, and the local-render qOS relay): a
//!   rectangular pixel surface. With no live transport we synthesize a
//!   representative desktop scene (wallpaper gradient + window cards +
//!   a taskbar) so the canvas, toolbar, and scaling controls have
//!   something concrete to frame.
//!
//! The actual byte transport (russh for SSH, an RFB client for VNC) is
//! intentionally *not* wired up here: live connections are gated behind
//! [`live_sessions_enabled`] so the app ships a populated, deterministic
//! offline experience and never blocks a render on the network.

use crate::model::ConnectionKind;
use crate::theme_ext::ColorExt;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill, Gradient};

/// Whether live network sessions are enabled. Off by default so the app
/// is fully usable (and screenshot-stable) without a transport. A future
/// build flips this on once the russh / RFB clients are wired through.
pub fn live_sessions_enabled() -> bool {
    std::env::var("REMOTE_LIVE_SESSIONS").is_ok()
}

/// One styled terminal cell. We keep this tiny — a glyph plus a palette
/// index — because a real ANSI grid is thousands of these.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub ch: char,
    /// Index into the toolkit's 16-colour ANSI palette, or `None` for the
    /// default foreground.
    pub color: Option<usize>,
    pub bold: bool,
}

impl Cell {
    fn plain(ch: char) -> Self {
        Self { ch, color: None, bold: false }
    }
}

/// A styled run within a transcript line — start column + the cells.
#[derive(Clone, Debug, Default)]
pub struct TermLine {
    pub cells: Vec<Cell>,
}

impl TermLine {
    /// Build a line from plain text (default colour).
    fn plain(s: &str) -> Self {
        Self { cells: s.chars().map(Cell::plain).collect() }
    }

    /// Build a line from `(text, color_index, bold)` segments.
    fn styled(segments: &[(&str, Option<usize>, bool)]) -> Self {
        let mut cells = Vec::new();
        for (text, color, bold) in segments {
            for ch in text.chars() {
                cells.push(Cell { ch, color: *color, bold: *bold });
            }
        }
        Self { cells }
    }
}

/// A terminal session model — a scrollback of [`TermLine`]s plus a live
/// prompt the (sample) input line renders into.
#[derive(Clone, Debug, Default)]
pub struct TerminalSession {
    pub lines: Vec<TermLine>,
    /// The current input the user is typing after the prompt.
    pub input: String,
    /// Rendered prompt prefix (e.g. `admin@build:~$`).
    pub prompt: String,
}

impl TerminalSession {
    /// A representative login transcript for `conn_label`, coloured with
    /// the ANSI palette so it reads as a genuine shell session offline.
    pub fn sample(host: &str, user: &str) -> Self {
        // ANSI palette indices (see TerminalPalette): 2 green, 4 blue,
        // 5 purple, 6 cyan, 3 yellow, 1 red, 7 light-grey.
        let green = Some(2usize);
        let blue = Some(4usize);
        let cyan = Some(6usize);
        let yellow = Some(3usize);
        let purple = Some(5usize);
        let dim = Some(8usize);

        let prompt = format!("{user}@{host}:~$ ");
        let lines = vec![
            TermLine::plain(&format!(
                "Last login: Mon Jun 30 09:14:22 2026 from 10.0.0.12"
            )),
            TermLine::styled(&[
                ("Welcome to ", None, false),
                ("Fedora bootc 42", blue, true),
                (" (qOS edition)", dim, false),
            ]),
            TermLine::default(),
            TermLine::styled(&[(&prompt, green, true), ("uname -a", None, false)]),
            TermLine::plain(
                "Linux build 7.0.9 #1 SMP PREEMPT_DYNAMIC x86_64 GNU/Linux",
            ),
            TermLine::styled(&[(&prompt, green, true), ("systemctl status quick-fs", None, false)]),
            TermLine::styled(&[
                ("  ", None, false),
                ("●", green, true),
                (" quick-fs.service - qOS projection filesystem", None, false),
            ]),
            TermLine::styled(&[
                ("     Loaded: loaded ", dim, false),
                ("(/usr/lib/systemd/system/quick-fs.service; enabled)", dim, false),
            ]),
            TermLine::styled(&[
                ("     Active: ", dim, false),
                ("active (running)", green, false),
                (" since Mon 2026-06-30 09:02:11", dim, false),
            ]),
            TermLine::styled(&[(&prompt, green, true), ("df -h /var/workspace", None, false)]),
            TermLine::styled(&[
                ("Filesystem      Size  Used Avail Use% Mounted on", cyan, false),
            ]),
            TermLine::plain("qos-fs           512G  187G  325G  37% /var/workspace"),
            TermLine::styled(&[(&prompt, green, true), ("git status", None, false)]),
            TermLine::styled(&[
                ("On branch ", None, false),
                ("main", purple, true),
            ]),
            TermLine::styled(&[
                ("Your branch is up to date with ", None, false),
                ("'origin/main'", cyan, false),
                (".", None, false),
            ]),
            TermLine::styled(&[
                ("Changes not staged for commit:", yellow, false),
            ]),
            TermLine::styled(&[
                ("  modified:   ", None, false),
                ("apps/remote/src/app.rs", red(), false),
            ]),
            TermLine::default(),
            TermLine::styled(&[(&prompt, green, true), ("tail -f /var/log/qos.log", None, false)]),
            TermLine::styled(&[
                ("[09:31:02] ", dim, false),
                ("INFO ", green, false),
                ("compositor: cluster 'workshop' attached peer studio-node", None, false),
            ]),
            TermLine::styled(&[
                ("[09:31:04] ", dim, false),
                ("INFO ", green, false),
                ("collab: gossip mesh has 3 peers", None, false),
            ]),
        ];
        Self { lines, input: "htop".to_string(), prompt }
    }
}

fn red() -> Option<usize> {
    Some(1usize)
}

/// A synthesized remote-desktop scene used for VNC / RDP / qOS-relay
/// surfaces when no live transport is present. Drawn directly into the
/// scene so the framebuffer canvas, toolbar, and scaling controls have a
/// concrete picture to frame.
pub struct FramebufferScene;

impl FramebufferScene {
    /// Paint a representative remote desktop into `rect`. `kind` shifts
    /// the wallpaper accent so VNC/RDP/qOS surfaces look distinct.
    pub fn draw(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        kind: ConnectionKind,
        theme: &Theme,
        text_renderer: &mut ui::text_render::TextRenderer,
    ) {
        // Clip to the framebuffer rect so nothing bleeds past the canvas.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, transform, &rect);

        // Wallpaper: a diagonal gradient tinted by the connection kind.
        let (a, b) = wallpaper_stops(kind, theme);
        let grad = Gradient::new_linear(
            Point::new(rect.x0, rect.y0),
            Point::new(rect.x1, rect.y1),
        )
        .with_stops([(0.0, a), (1.0, b)]);
        scene.fill(Fill::NonZero, transform, &grad, None, &rect);

        // A few "open windows" — translucent cards with a title bar.
        let cards = [
            (0.06, 0.10, 0.42, 0.50, "Terminal"),
            (0.40, 0.28, 0.52, 0.58, "Files"),
            (0.20, 0.46, 0.40, 0.40, "Editor"),
        ];
        for (fx, fy, fw, fh, title) in cards {
            let card = Rect::new(
                rect.x0 + rect.width() * fx,
                rect.y0 + rect.height() * fy,
                rect.x0 + rect.width() * (fx + fw),
                rect.y0 + rect.height() * (fy + fh),
            );
            let rr = RoundedRect::from_rect(card, 8.0);
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(15, 23, 42, 235),
                None,
                &rr,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_white_20,
                None,
                &rr,
            );
            // Title bar.
            let bar = Rect::new(card.x0, card.y0, card.x1, card.y0 + 26.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &RoundedRect::from_rect(bar, 8.0),
            );
            // Traffic-light dots.
            for (i, dot) in [theme.tokens.colors.semantic.danger,
                theme.tokens.colors.semantic.warning,
                theme.tokens.colors.semantic.success]
                .into_iter()
                .enumerate()
            {
                let cx = card.x0 + 14.0 + i as f64 * 14.0;
                let cy = card.y0 + 13.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    dot,
                    None,
                    &vello::kurbo::Circle::new(Point::new(cx, cy), 4.0),
                );
            }
            text_renderer.draw_text(
                scene,
                title,
                12.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((card.x0 + 64.0, card.y0 + 6.0)),
                None,
            );
        }

        // Taskbar across the bottom.
        let taskbar = Rect::new(rect.x0, rect.y1 - 36.0, rect.x1, rect.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 160),
            None,
            &taskbar,
        );
        text_renderer.draw_text(
            scene,
            &format!("{} desktop · 1920×1080", kind.label()),
            12.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((rect.x0 + 12.0, rect.y1 - 26.0)),
            None,
        );

        scene.pop_layer();
    }
}

fn wallpaper_stops(kind: ConnectionKind, theme: &Theme) -> (Color, Color) {
    let accent = theme.tokens.colors.accent[5];
    match kind {
        ConnectionKind::Vnc => (
            Color::from_rgb8(30, 41, 59),
            accent.darken(0.45),
        ),
        ConnectionKind::Rdp => (
            Color::from_rgb8(20, 30, 60),
            Color::from_rgb8(8, 47, 73),
        ),
        // qOS relay: the local-render kind. Tint toward the qOS indigo so
        // it reads as "our own surface, relayed".
        ConnectionKind::Qos => (accent.darken(0.55), accent.darken(0.2)),
        ConnectionKind::Ssh => (
            Color::from_rgb8(15, 23, 42),
            Color::from_rgb8(30, 41, 59),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_transcript_is_populated() {
        let t = TerminalSession::sample("build", "admin");
        assert!(t.lines.len() > 10);
        assert!(t.prompt.contains("admin@build"));
    }

    #[test]
    fn live_sessions_default_off() {
        // The default offline build must not attempt a live transport.
        unsafe {
            std::env::remove_var("REMOTE_LIVE_SESSIONS");
        }
        assert!(!live_sessions_enabled());
    }
}
