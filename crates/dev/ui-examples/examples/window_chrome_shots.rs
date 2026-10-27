#![allow(
    clippy::exit,
    reason = "a capture scenario reports an unusable output directory as its exit status"
)]

//! Window chrome in dark and light: a modal whose scrim covers the whole
//! window (the History bar included), an option menu and a content popover,
//! the status surfaces, and video controls on the floating material.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- window_chrome [OUT_DIR]`
//! Out:  `OUT_DIR` (default `/tmp/window_chrome`): `modal-<theme>.png`,
//!       `popover-<theme>.png`, `status-<theme>.png`, `video-<theme>.png`.

use std::path::{Path, PathBuf};

use tilo_ui::State;
use tilo_ui::TextRole;
use tilo_ui::headless::FrameHarness;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::time::Duration;
use tilo_ui::widget::video_player::PlayerSetting;
use tilo_ui::widget::{
    Banner, BannerContent, BannerKind, Column, ModalDialog, Popover, PopoverSide, Text,
    VideoPlayer, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

const SETTLE: Duration = Duration::from_secs(2);

pub fn run(args: &[String]) {
    let out = args
        .first()
        .map_or_else(|| PathBuf::from("/tmp/window_chrome"), PathBuf::from);
    if let Err(error) = std::fs::create_dir_all(&out) {
        eprintln!("window_chrome: cannot create {}: {error}", out.display());
        std::process::exit(2);
    }
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        modal(&out, name, &theme);
        popover(&out, name, &theme);
        status(&out, name, &theme);
        video(&out, name, &theme);
    }
}

/// A short document page, the ground the overlays stand over.
fn page() -> Column {
    Column::new()
        .push(Box::new(
            Text::new("Quarterly report").with_role(TextRole::LargeTitle),
        ))
        .push(Box::new(Text::new(
            "Revenue grew in every region this quarter. The detail below breaks it down by \
             product line and by month, with the forecast for the next two quarters.",
        )))
        .push(Box::new(
            Text::new("Product lines").with_role(TextRole::Heading),
        ))
        .push(Box::new(Text::new(
            "Subscriptions carried most of the growth; hardware held steady.",
        )))
}

fn capture(out: &Path, file: &str, root: Box<dyn Widget>, theme: &Theme, history: bool) {
    let mut harness = FrameHarness::new_with_theme(State::new(), root, (1000, 640), theme.clone());
    if history {
        harness.enable_document_history_chrome();
    }
    harness.settle_for_capture(SETTLE);
    harness.render_png(&out.join(file));
}

fn modal(out: &Path, name: &str, theme: &Theme) {
    let root = page().push(Box::new(
        ModalDialog::new(
            "Discard this draft?",
            Text::new("Your changes to Quarterly report since 10:12 have not been saved."),
        )
        .open(true)
        .secondary_action("Keep editing", || {})
        .with_destructive(true)
        .primary_action("Discard draft", || {}),
    ));
    capture(
        out,
        &format!("modal-{name}.png"),
        Box::new(root),
        theme,
        true,
    );
}

fn popover(out: &Path, name: &str, theme: &Theme) {
    let mut menu = Popover::new().with_label("Refresh interval").with_items([
        "Every second",
        "Every 2 seconds",
        "Every 5 seconds",
        "Every minute",
    ]);
    menu.open_at(Rect::new(24.0, 150.0, 220.0, 182.0), Some(1));
    let detail = Popover::with_content(Text::new("Shared with Ana, Ben and Chloé"))
        .anchored(Rect::new(640.0, 150.0, 760.0, 182.0))
        .side(PopoverSide::Below)
        .open(true);
    let root = page().push(Box::new(menu)).push(Box::new(detail));
    capture(
        out,
        &format!("popover-{name}.png"),
        Box::new(root),
        theme,
        false,
    );
}

fn status(out: &Path, name: &str, theme: &Theme) {
    let (w, h) = (1000_u32, 640_u32);
    let window = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let (content, bar) = tilo_ui::chrome::status_bar::split(window, theme);
    let banner_rect = Rect::new(
        content.x0 + 24.0,
        content.y0 + 24.0,
        content.x1 - 24.0,
        content.y0 + 92.0,
    );
    Banner::paint(
        &mut tilo_ui::paint::PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, theme),
        banner_rect,
        &BannerContent::new(BannerKind::Warning, "Changes could not be synced")
            .with_message(Some("Tilo will retry when the connection is back."))
            .dismissible(true),
        vello::kurbo::Point::ZERO,
    );
    tilo_ui::chrome::status_bar::draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        bar,
        "1,204 words",
        "Ln 12, Col 4",
    );
    let path = out.join(format!("status-{name}.png"));
    OffscreenRenderer::new(w, h).render_png(&scene, theme.tokens.colors.surfaces.content, &path);
}

fn video(out: &Path, name: &str, theme: &Theme) {
    let mut player = VideoPlayer::new().with_setting(PlayerSetting::toggle(
        "captions",
        tilo_ui::icons::Icon::FileText,
        "Captions",
        1,
        true,
    ));
    player.reveal_controls();
    let root = Column::new().push(Box::new(player));
    let harness =
        FrameHarness::new_with_theme(State::new(), Box::new(root), (960, 540), theme.clone());
    harness.render_png(&out.join(format!("video-{name}.png")));
}
