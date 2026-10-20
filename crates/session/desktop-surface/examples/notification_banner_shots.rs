//! Fixture-only captures of the production live notification banner.
use std::cell::{Ref, RefCell};
use std::path::PathBuf;
use tilo_desktop_surface::HostAction;
use tilo_desktop_surface::notifications::{
    HistoryActivation, Notification, NotificationHost, NotificationLog, NotificationSeverity,
    NotificationStack,
};
use ui::offscreen::OffscreenRenderer;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::{
    Scene,
    kurbo::{Affine, Rect},
};
const HEIGHT: u32 = 460;
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
    // As the browser desktop answers: an ask's choices, the answer last.
    fn actions_for(&self, notice: &Notification) -> Vec<(String, String)> {
        if tilo_desktop_surface::is_host_offer(notice) {
            notice.actions.iter().rev().cloned().collect()
        } else {
            notice.actions.clone()
        }
    }
    fn dismiss(&self, _: u64) {}
    fn activate(&self, _: u64) {}
    fn activate_history(&self, _: u64) -> HistoryActivation {
        HistoryActivation::Unavailable
    }
    fn dismiss_history(&self, _: u64) {}
    fn clear_history(&self) {}
    fn undo_clear(&self) {}
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args().nth(1).map_or_else(
        || PathBuf::from("target/notification-banner-shots"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out)?;
    let variant = std::env::args().nth(2);
    // Boxed: six themes are too large to build on the stack.
    for (name, theme, width) in vec![
        ("dark", Theme::dark(), 440_u32),
        ("light", Theme::light(), 440),
        ("large-text", Theme::dark().with_text_scale(2.0), 440),
        (
            "high-contrast",
            Theme::high_contrast().with_text_scale(2.0),
            440,
        ),
        (
            "ui-text",
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
            640,
        ),
        ("narrow", Theme::light().with_text_scale(2.0), 320),
    ] {
        let theme = theme.with_reduce_motion(true);
        let host = Fixture::default();
        // What a person meets: something a collaborator did, an ask that
        // waits for an answer, and a failure they did not cause.
        let notices = if variant.as_deref() == Some("error") {
            vec![
                Notification::new("Couldn't open Notes", "Try again from Search.")
                    .with_app_name("Applications")
                    .with_severity(NotificationSeverity::Error),
            ]
        } else if variant.as_deref() == Some("not-saved") {
            vec![not_saved()?]
        } else if variant.as_deref() == Some("overflow") {
            // More than fit: the rest fold into one card that opens the
            // centre, and the newest holds keyboard focus.
            (1..=6)
                .map(|n| {
                    Notification::new(format!("Build {n} finished"), "All tests passed")
                        .with_app_name("Terminal")
                })
                .collect()
        } else {
            let mut ask = HostAction {
                id: "keep-identity".into(),
                title: "Keep with passkey".into(),
                subtitle: None,
                source: None,
                label: None,
            }
            .notice(
                "Keep this identity?",
                "A passkey brings you back as yourself on any browser. Any time later: Ctrl+K, “Keep this identity”.",
            );
            ask.app_name = None;
            vec![
                Notification::new("Ada mentioned you", "“Can you look at the launch notes?”")
                    .with_app_name("Chat")
                    .with_from("Ada", "peer:ada"),
                ask,
                Notification::new(
                    "Sync paused",
                    "This browser went offline. Changes are kept here.",
                )
                .with_severity(NotificationSeverity::Warning),
            ]
        };
        for notice in notices {
            if !host.0.borrow_mut().show(notice).is_empty() {
                return Err("capture notice unexpectedly evicted".into());
            }
        }
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, f64::from(width), f64::from(HEIGHT)));
        let mut scene = Scene::new();
        stack.draw(
            &host,
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
        );
        if variant.as_deref() == Some("overflow") && stack.focus_newest(&host) {
            scene = Scene::new();
            stack.draw(
                &host,
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
            );
        }
        OffscreenRenderer::new(width, HEIGHT).render_png(
            &scene,
            theme.tokens.colors.surfaces.base,
            out.join(format!("{name}.png")),
        );
    }
    Ok(())
}

/// A note's save that gave up while it was out of sight, as the desktop
/// shows the notification Notes raises: named, a warning, with the one
/// choice the note gives.
fn not_saved() -> Result<Notification, Box<dyn std::error::Error>> {
    let t0 = ui::time::Instant::now();
    let mut surface = ui::SaveStatusSurface::new();
    surface.set(ui::SaveState::Saving, t0);
    let late = t0
        .checked_add(ui::SAVE_GIVE_UP)
        .ok_or("the give-up instant is out of range")?;
    let _ = surface.take_notice(late, Some("Launch checklist"), "Notes");
    let unseen = late
        .checked_add(ui::SAVE_STALL)
        .ok_or("the unseen instant is out of range")?;
    let notice = surface
        .take_notice(unseen, Some("Launch checklist"), "Notes")
        .ok_or("a save nobody saw give up is notified")?;
    let mut shown = Notification::new(notice.title, notice.body)
        .with_app_name("Notes")
        .with_severity(NotificationSeverity::Warning);
    shown.actions = vec![("default".into(), notice.action)];
    Ok(shown)
}
