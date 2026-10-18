//! Production notification-reader captures, with fixture-only notices.
//! Run: cargo run -p tilo-desktop-surface --example `notification_reader_shots`
//! The optional first argument is the output directory.
use std::cell::{Ref, RefCell};
use std::path::PathBuf;
use tilo_desktop_surface::notifications::{
    CenterLayout, HistoryActivation, Notification, NotificationCenter, NotificationHost,
    NotificationLog, NotificationSeverity,
};
use ui::offscreen::OffscreenRenderer;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::keyboard::{Key, NamedKey};

#[derive(Default)]
struct Fixture(RefCell<NotificationLog>);

impl NotificationHost for Fixture {
    fn log(&self) -> Ref<'_, NotificationLog> {
        self.0.borrow()
    }
    fn present(&self, now: ui::time::Instant, on_screen: &[u64], held: bool) {
        self.0.borrow_mut().present(now, on_screen, held);
    }
    fn open_center(&self) {}
    fn dismiss(&self, id: u64) {
        self.dismiss_history(id);
    }
    fn activate(&self, id: u64) {
        self.dismiss_history(id);
    }
    fn activate_history(&self, _id: u64) -> HistoryActivation {
        HistoryActivation::Unavailable
    }
    fn dismiss_history(&self, id: u64) {
        self.0.borrow_mut().take_history(id);
    }
    fn clear_history(&self) {
        self.0.borrow_mut().clear_history(ui::time::now());
    }
    fn undo_clear(&self) {
        self.0.borrow_mut().undo_clear();
    }
}

/// A background failure with a full explanation: the reader is
/// where a notification too long for its row is read.
const REPORT: &str = concat!(
    "Last night's backup to “Backup Disk” stopped at 02:14, 62 % of the way through, ",
    "when the disk disconnected.\n\n",
    "Error: the disk went away while it was being written to (input/output error on ",
    "Backup Disk, sdb1).\n\n",
    "Nothing on this computer changed, and the earlier backups on the disk are intact. ",
    "Saved: Documents, Pictures and Notes up to 02:14. Not yet saved: Music, Videos and ",
    "anything changed since.\n\n",
    "What to do\n",
    "• Reconnect the disk. The next backup starts on its own and picks up where this one ",
    "stopped, so nothing is copied twice.\n",
    "• If it disconnects again, try another cable or port: a disk that loses power during ",
    "a backup can take a minute to come back.\n",
    "• Backups checks every file it copies, so an interrupted backup never leaves a ",
    "damaged copy behind.\n\n",
    "Last complete backup: yesterday at 02:00."
);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args().nth(1).map_or_else(
        || PathBuf::from("target/notification-reader-shots"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out)?;
    let variants = vec![
        ("dark", Theme::dark(), 560_u32, 560_u32),
        ("light", Theme::light(), 400, 560),
        ("high-contrast", Theme::high_contrast(), 400, 560),
        ("large-text", Theme::dark().with_text_scale(2.0), 560, 560),
        (
            "ui-text",
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
            640,
            560,
        ),
        ("narrow", Theme::light().with_text_scale(1.3), 320, 480),
    ];
    for (name, theme, width, height) in variants {
        let host = Fixture::default();
        host.0.borrow_mut().retain(
            Notification::new("Backup couldn't finish", REPORT)
                .with_app_name("Backups")
                .with_severity(NotificationSeverity::Error),
        );
        let mut center = NotificationCenter::new();
        let mut text = TextRenderer::new();
        let renderer = &mut OffscreenRenderer::new(width, height);
        for state in [
            "row-focused",
            "reading-start",
            "reading-end",
            "back-focused",
            "resized",
            "pointer-opened",
        ] {
            let inset = 16.0;
            let panel_width = if state == "resized" {
                f64::from(width) - 64.0
            } else {
                f64::from(width) - 32.0
            };
            let panel = Rect::new(inset, inset, inset + panel_width, f64::from(height) - inset);
            let layout = CenterLayout::new(panel, CenterLayout::metrics(&theme, &mut text));
            match state {
                "row-focused" => {
                    center.focus_accessibility_path(&host, &layout, &[0, 0]);
                }
                "reading-start" => {
                    center.key(&host, &layout, &Key::Named(NamedKey::Enter));
                }
                "reading-end" => {
                    center.key(&host, &layout, &Key::Named(NamedKey::End));
                }
                "back-focused" => {
                    center.focus_accessibility_path(&host, &layout, &[0]);
                }
                "pointer-opened" => {
                    // Opened by the pointer: no focus ring until the
                    // keyboard moves focus.
                    center = NotificationCenter::new();
                    center.click(&host, &layout, layout.row(0).center());
                }
                _ => {}
            }
            let mut scene = Scene::new();
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                ui::chrome::panel_ground(&theme),
                None,
                &RoundedRect::from_rect(panel, f64::from(theme.tokens.radii.panel)),
            );
            center.draw(
                &host,
                &layout,
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                None,
            );
            let path = out.join(format!("{name}-{state}.png"));
            renderer.render_png(&scene, theme.tokens.colors.surfaces.base, &path);
            println!("{}", path.display());
        }
    }
    Ok(())
}
