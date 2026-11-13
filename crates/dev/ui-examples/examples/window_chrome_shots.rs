#![allow(
    clippy::exit,
    reason = "a capture scenario reports an unusable output directory as its exit status"
)]

//! Window chrome in dark and light: a modal whose scrim covers the whole
//! window (the History bar included), an option menu and a content popover,
//! a transient notice in the window's bar, a sheet up from the foot, the
//! status surfaces, and video controls on the floating material.
//! Run:  `cargo run -p ui-examples --example ui-shots -- window_chrome [OUT_DIR]`

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
        app_menu(&out, name, &theme);
        notice(&out, name, &theme);
        sheet(&out, name, &theme);
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
        harness.present_history_in_header();
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

/// The option menus two apps open from a button: the camera chooser (devices,
/// the one in use checked) and the calendar's repeat-scope question.
fn app_menu(out: &Path, name: &str, theme: &Theme) {
    let mut camera = Popover::new().with_label("Camera").with_items([
        "Built-in camera",
        "Logitech BRIO",
        "Continuity Camera",
    ]);
    camera.open_at(Rect::new(24.0, 150.0, 244.0, 182.0), Some(1));
    let mut repeat = Popover::new()
        .with_label("Change repeat")
        .with_items(["Only this event", "All future events"]);
    repeat.open_at(Rect::new(560.0, 150.0, 700.0, 182.0), Some(1));
    let root = page().push(Box::new(camera)).push(Box::new(repeat));
    capture(
        out,
        &format!("app-menu-{name}.png"),
        Box::new(root),
        theme,
        false,
    );
}

/// A transient notice in the window's bar: an operation's result, work under way or a failure,
/// shown where the person is looking, one at a time, beside the app's own slots.
fn notice(out: &Path, name: &str, theme: &Theme) {
    use tilo_ui::widget::toast::ToastKind;
    use tilo_ui::window_toolbar::{ToolbarAction, ToolbarNotice, notify, withdraw};
    let up = ToolbarNotice::new("Changes could not be synced", ToastKind::Warning)
        .action(ToolbarAction::new("retry", "refresh", "Retry"));
    notify(Some(up.clone()));
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(page()), (1000, 640), theme.clone());
    harness.present_in_window_bar();
    harness.settle_for_capture(SETTLE);
    harness.render_png(&out.join(format!("notice-{name}.png")));
    withdraw(&up);
}

/// A sheet up from the foot of a narrow window, resting at half height,
/// over the content it belongs to.
fn sheet(out: &Path, name: &str, theme: &Theme) {
    use tilo_ui::widget::{BottomSheet, SheetDetent, SheetFrame};
    let (w, h) = (420_u32, 640_u32);
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let body = theme.text(TextRole::Body);
    tr.draw_style(
        &mut scene,
        "Inbox",
        theme.tokens.colors.text_primary,
        Affine::translate((24.0, 40.0)),
        None,
        &theme.text(TextRole::LargeTitle),
    );
    tr.draw_style(
        &mut scene,
        "Ana asked about the forecast sheet.",
        theme.tokens.colors.text_dim,
        Affine::translate((24.0, 96.0)),
        None,
        &body,
    );
    let mut sheet = BottomSheet::new();
    sheet.snap_to(SheetDetent::Half);
    let frame = SheetFrame {
        area: Rect::new(0.0, 120.0, f64::from(w), f64::from(h)),
        peek: 140.0,
        grip: f64::from(theme.tokens.metrics.min_pointer_target),
        least_half: 0.0,
    };
    let placed = sheet.place(frame, 1.0, true);
    let on_sheet = sheet.paint(&mut scene, Affine::IDENTITY, theme);
    tr.draw_style(
        &mut scene,
        "Workspace",
        on_sheet.tokens.colors.text_primary,
        Affine::translate((placed.content.x0 + 24.0, placed.content.y0 + 16.0)),
        None,
        &on_sheet.text(TextRole::Heading),
    );
    let mut y = placed.content.y0 + 60.0;
    for label in ["Members", "Devices", "Storage"] {
        tr.draw_style(
            &mut scene,
            label,
            on_sheet.tokens.colors.text_primary,
            Affine::translate((placed.content.x0 + 24.0, y)),
            None,
            &on_sheet.text(TextRole::Body),
        );
        y += 44.0;
    }
    let path = out.join(format!("sheet-{name}.png"));
    OffscreenRenderer::new(w, h).render_png(&scene, theme.tokens.colors.surfaces.content, &path);
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
