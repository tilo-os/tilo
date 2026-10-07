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
        let body = concat!(
            "The app stopped before opening. Try again. Your document is safe.\n\n",
            "If it stops again, reopen the app from Search. Changes already synchronized remain available on this device. ",
            "You can return to your workspace and continue with another app while this one recovers.\n\n",
            "Recovery step 1: close the failed launch and open Search. Enter the application name and select its result. ",
            "If the app is still downloading, let that download finish before trying again.\n\n",
            "Recovery step 2: check whether the document is available in Files. Keep the original document while recovery completes. ",
            "An interrupted launch does not require deleting the document or clearing notification history.\n\n",
            "Recovery step 3: if the issue continues, keep this complete message for troubleshooting. ",
            "This reading page can be scrolled using the pointer, Page Down, End or accessible scrolling.\n\n",
            "Recovery complete: your document is safe."
        );
        host.0.borrow_mut().retain(
            Notification::new("Couldn't open Notes — complete recovery instructions", body)
                .with_app_name("Applications")
                .with_severity(NotificationSeverity::Error),
        );
        let mut center = NotificationCenter::new();
        let mut text = TextRenderer::new();
        let renderer = &mut OffscreenRenderer::new(width, height);
        for state in [
            "read-focused",
            "reading-start",
            "reading-end",
            "back-focused",
            "resized",
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
                "read-focused" => {
                    center.focus_accessibility_path(&host, &[0, 0, 1]);
                }
                "reading-start" => {
                    center.key(&host, &layout, &Key::Named(NamedKey::Enter));
                }
                "reading-end" => {
                    center.key(&host, &layout, &Key::Named(NamedKey::End));
                }
                "back-focused" => {
                    center.focus_accessibility_path(&host, &[0]);
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
