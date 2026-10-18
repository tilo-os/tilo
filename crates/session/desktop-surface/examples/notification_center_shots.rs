//! Production notification-center captures, with fixture-only notices.
//! Run: cargo run -p tilo-desktop-surface --example `notification_center_shots`
//! The optional first argument is the output directory.
use std::cell::{Ref, RefCell};
use std::path::PathBuf;
use tilo_desktop_surface::notifications::{
    CenterLayout, HistoryActivation, Notification, NotificationCenter, NotificationHost,
    NotificationLog, NotificationSeverity,
};
use tilo_desktop_surface::{HostAction, HostActionSource};
use ui::offscreen::OffscreenRenderer;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::keyboard::{Key, NamedKey};

const HEIGHT: u32 = 640;

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
    fn actions_for(&self, notice: &Notification) -> Vec<(String, String)> {
        notice.actions.clone()
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

/// The browser host's ask to keep the person's identity, as it posts it:
/// sourced to Identity, with its choices.
fn ask() -> Notification {
    let keep = HostAction {
        id: "keep-identity".into(),
        title: "Keep with passkey".into(),
        subtitle: None,
        source: Some(HostActionSource::Identity),
        label: None,
    };
    let mut ask = keep.notice(
        "Keep this identity?",
        "A passkey brings you back as yourself on any browser.",
    );
    ask.actions.insert(0, ("not-now".into(), "Not now".into()));
    ask
}

/// What a centre legitimately holds: things the person did not do
/// themselves, retained oldest first, as they arrived.
fn fixture() -> Fixture {
    let host = Fixture::default();
    for notice in [
        Notification::new(
            "Backup couldn't finish",
            "The backup disk was disconnected halfway through. Reconnect it and the next backup picks up where this one stopped.",
        )
        .with_app_name("Backups")
        .with_severity(NotificationSeverity::Error),
        ask(),
        Notification::new("Lena", "Pushed the new floor plan, take a look when you can")
            .with_app_name("Chat"),
        Notification::new(
            "Ada mentioned you in Launch notes",
            "“Can you check the pricing table before we send this out tomorrow morning?”",
        )
        .with_app_name("Notes"),
    ] {
        host.0.borrow_mut().retain(notice);
    }
    host
}

/// Focus the dialog control drawn at `rect`, as an assistive technology
/// would by its path.
fn focus_at(
    center: &mut NotificationCenter,
    host: &dyn tilo_desktop_surface::notifications::NotificationHost,
    layout: &CenterLayout,
    rect: Rect,
) {
    let dialog = center.accessibility(host, layout, ui::time::now());
    if let Some(index) = dialog
        .children
        .iter()
        .position(|node| node.bounds.map(ui::a11y::AccessBounds::to_rect) == Some(rect))
    {
        center.focus_accessibility_path(host, layout, &[index]);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args().nth(1).map_or_else(
        || PathBuf::from("target/notification-center-shots"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out)?;
    for (name, theme) in capture_themes() {
        let host = fixture();
        let width = if name.starts_with("narrow-") {
            352
        } else if name == "large-text" {
            560
        } else {
            400
        };
        let metrics = CenterLayout::metrics(&theme, &mut TextRenderer::new());
        // As tall as the rows ask within the room, like every host sizes it.
        let height = NotificationCenter::new().fitted_height(
            &host,
            &metrics,
            f64::from(width) - 32.0,
            f64::from(HEIGHT) - 32.0,
        );
        let panel = Rect::new(16.0, 16.0, f64::from(width) - 16.0, 16.0 + height);
        let layout = CenterLayout::new(panel, metrics);
        let mut center = NotificationCenter::new();
        for state in [
            "rest",
            "hover",
            "clear-focused",
            "back-focused",
            "dismiss-focused",
            "older-focused",
            "undo-focused",
            "restored",
        ] {
            let mut cursor = None;
            match state {
                "rest" => {}
                "hover" => cursor = Some(layout.row(0).center()),
                "clear-focused" => {
                    center.focus_accessibility_path(&host, &layout, &[1]);
                }
                "back-focused" => {
                    focus_at(&mut center, &host, &layout, layout.back());
                }
                "older-focused" => {
                    focus_at(&mut center, &host, &layout, layout.pager().1);
                }
                "dismiss-focused" => {
                    center.focus_accessibility_path(&host, &layout, &[0, 0, 0]);
                }
                "undo-focused" => {
                    center.click(&host, &layout, layout.clear().center());
                    center.focus_accessibility_path(&host, &layout, &[2]);
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
                cursor,
            );
            let path = out.join(format!("{name}-{state}.png"));
            OffscreenRenderer::new(width, HEIGHT).render_png(
                &scene,
                theme.tokens.colors.surfaces.base,
                &path,
            );
            println!("{}", path.display());
        }
    }
    Ok(())
}
