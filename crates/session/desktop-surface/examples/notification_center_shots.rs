//! Production notification-center captures, with fixture-only notices.
//! Run: cargo run -p tilo-desktop-surface --example `notification_center_shots`
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

fn capture_themes() -> Vec<(&'static str, Theme)> {
    vec![
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("large-text", Theme::dark().with_text_scale(2.0)),
        ("narrow-dark", Theme::dark().with_text_scale(2.0)),
        ("narrow-light", Theme::light().with_text_scale(2.0)),
        (
            "narrow-contrast",
            Theme::high_contrast().with_text_scale(2.0),
        ),
        (
            "narrow-combined",
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
        ),
    ]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args().nth(1).map_or_else(
        || PathBuf::from("target/notification-center-shots"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out)?;
    for (name, theme) in capture_themes() {
        let host = Fixture::default();
        host.0.borrow_mut().retain(
            Notification::new(
                "Couldn't open Notes",
                "The app stopped before opening. Try again.",
            )
            .with_app_name("Applications")
            .with_severity(NotificationSeverity::Error),
        );
        host.0.borrow_mut().retain(
            Notification::new(
                "Document synchronized",
                "All changes are available on this device.",
            )
            .with_app_name("Files"),
        );
        let width = if name.starts_with("narrow-") {
            352
        } else if name == "large-text" {
            560
        } else {
            400
        };
        let panel = Rect::new(16.0, 16.0, f64::from(width) - 16.0, 544.0);
        let layout = CenterLayout::new(
            panel,
            CenterLayout::metrics(&theme, &mut TextRenderer::new()),
        );
        let mut center = NotificationCenter::new();
        for state in [
            "clear-focused",
            "back-focused",
            "dismiss-focused",
            "undo-focused",
            "restored",
        ] {
            match state {
                "clear-focused" => {
                    center.focus_accessibility_path(&host, &[1]);
                }
                "back-focused" => {
                    let dialog = center.accessibility(&host, &layout, ui::time::now());
                    center.focus_accessibility_path(
                        &host,
                        &[dialog.children.len().saturating_sub(1)],
                    );
                }
                "dismiss-focused" => {
                    center.focus_accessibility_path(&host, &[0, 0, 0]);
                }
                "undo-focused" => {
                    center.click(&host, &layout, layout.clear().center());
                    center.focus_accessibility_path(&host, &[2]);
                }
                _ => {
                    center.key(&host, &layout, &Key::Named(NamedKey::Enter));
                }
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
                &mut TextRenderer::new(),
                &theme,
                None,
            );
            let path = out.join(format!("{name}-{state}.png"));
            OffscreenRenderer::new(width, 560).render_png(
                &scene,
                theme.tokens.colors.surfaces.base,
                &path,
            );
            println!("{}", path.display());
        }
    }
    Ok(())
}
