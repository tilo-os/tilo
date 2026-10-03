//! Render the production launcher-failure banner at common laptop sizes.
//!
//! ```text
//! cargo run -p shell --example launcher_error_shots
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use shell::shell::chrome_state::ChromeState;
use shell::shell::notification::ChromeNotifications;
use shell::shell::state::WorkspaceTree;
use tilo_desktop::notifications::{Notification, NotificationSeverity, NotificationStack};
use ui::offscreen::OffscreenRenderer;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;

fn shot(out: &Path, width: u32, height: u32) {
    let theme = Theme::dark();
    let chrome = ChromeState::new(WorkspaceTree::default());
    chrome.borrow_mut().push_notification_full(
        Notification::new(
            "Couldn't open ChatGPT",
            "The app stopped before opening. Try again.",
        )
        .with_app_name("Applications")
        .with_severity(NotificationSeverity::Error)
        .with_created_at(
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
        )
        .with_dismiss_after(Duration::from_secs(8)),
    );

    let viewport = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.base,
        None,
        &viewport,
    );
    // Quiet desktop context makes the top-right placement and edge spacing
    // reviewable without duplicating the compositor's complete scene graph.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.raised,
        None,
        &Rect::new(0.0, 0.0, viewport.x1, 40.0),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.neutral[2],
        None,
        &RoundedRect::from_rect(
            Rect::new(
                48.0,
                88.0,
                (viewport.x1 - 48.0).max(49.0),
                viewport.y1 - 48.0,
            ),
            18.0,
        ),
    );

    let mut stack = NotificationStack::new();
    stack.set_viewport(viewport);
    stack.draw(
        &ChromeNotifications::new(&chrome),
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
    );
    OffscreenRenderer::new(width, height).render_png(
        &scene,
        theme.tokens.colors.surfaces.base,
        out,
    );
    println!("wrote {}", out.display());
}

fn main() -> Result<(), String> {
    ui::i18n::I18nManager::init("en-US");
    let dir = PathBuf::from(
        std::env::var("LAUNCHER_ERROR_SHOTS_OUT")
            .unwrap_or_else(|_| "target/launcher-error-shots".into()),
    );
    std::fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    for (width, height) in [(1600, 900), (1366, 768), (1280, 720)] {
        shot(
            &dir.join(format!("launcher-error-{width}x{height}.png")),
            width,
            height,
        );
    }
    Ok(())
}
