//! The send-to box: the one a menu opens beside it, and the one a report
//! opens at the one place reports go — no recipient to search for, the
//! destination on its chip, and the words left to write.

use tilo_ui::TextRole;
use tilo_ui::floating_surface::{Recipient, RecipientKind};
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::text_render::{TextRenderer, TextWeight};
use tilo_ui::theme::Theme;
use tilo_ui::widget::SendToBox;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;

pub fn run(_: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    // One frame of a capture sees the box at rest, where it arrives.
    let _settled = tilo_ui::motion::testing::settled();
    let theme = crate::registry::shot_theme();
    let out = tilo_ui::env::var("TILO_UI_SNAPSHOT_OUT")
        .unwrap_or_else(|_| "/tmp/tilo-send-to.png".into());
    let mut renderer = OffscreenRenderer::new(1280, 720);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let c = &theme.tokens.colors;
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        c.surfaces.base,
        None,
        &Rect::new(0.0, 0.0, 1280.0, 720.0),
    );

    // What each box is opened over: a window, its menu, and the box beside it.
    for (title, window, menu) in [
        (
            "Send to an agent",
            Rect::new(24.0, 48.0, 600.0, 672.0),
            Rect::new(150.0, 200.0, 300.0, 380.0),
        ),
        (
            "Report a problem",
            Rect::new(680.0, 48.0, 1256.0, 672.0),
            Rect::new(700.0, 200.0, 850.0, 380.0),
        ),
    ] {
        shadowed(&mut scene, &theme, window);
        tr.draw_style(
            &mut scene,
            title,
            c.text_dim,
            Affine::translate((window.x0 + 22.0, window.y0 - 20.0)),
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Medium),
        );
        menu_panel(&mut scene, &theme, menu);
    }

    // The box a menu opens: about what the menu acts on, to the agent the
    // message makes, with the field that names someone else.
    let mut addressed_to_an_agent = SendToBox::new();
    addressed_to_an_agent.set_viewport(Some(Rect::new(24.0, 48.0, 600.0, 672.0)));
    addressed_to_an_agent.set_about(Some(tilo_ui::shared_selection::SharedSelection::of_text(
        "Invoice from Ada, due Friday",
    )));
    addressed_to_an_agent.open(None, &[]);
    let _ = addressed_to_an_agent.perform_semantic_action(
        "send-to:message",
        &tilo_ui::a11y::Action::SetValue(
            "Ada, the invoice from Friday still carries the old address. Can you check what we \
             have on file, and whether the March batch went out with the same one?"
                .to_owned(),
        ),
    );
    // Placed once the words are in: the box makes room for what they wrap to.
    addressed_to_an_agent.place(Rect::new(150.0, 200.0, 300.0, 380.0), None);
    addressed_to_an_agent.paint(&mut scene, Affine::IDENTITY, &mut tr, &theme);

    // The box a report opens: the one place reports go on its chip, no search
    // field, and the words the person writes.
    let destination = Recipient::found(RecipientKind::Feedback, "c_feedback", "Tilo feedback");
    let mut addressed_to_reports = SendToBox::new();
    addressed_to_reports.set_viewport(Some(Rect::new(680.0, 48.0, 1256.0, 672.0)));
    addressed_to_reports.set_about(Some(tilo_ui::shared_selection::SharedSelection::of_text(
        "Invoice from Ada, due Friday",
    )));
    addressed_to_reports.open(None, std::slice::from_ref(&destination));
    let _ = addressed_to_reports.perform_semantic_action(
        "send-to:message",
        &tilo_ui::a11y::Action::SetValue(
            "The invoice button does nothing the second time I press it, and the page then \
             forgets which workspace I was in. It happened again when I reopened the same file \
             a minute later."
                .to_owned(),
        ),
    );
    addressed_to_reports.place(Rect::new(700.0, 200.0, 850.0, 380.0), None);
    addressed_to_reports.paint(&mut scene, Affine::IDENTITY, &mut tr, &theme);

    renderer.render_png(&scene, c.surfaces.base, out);
}

/// A window standing on the desk, so each box is seen where it opens.
fn shadowed(scene: &mut Scene, theme: &Theme, rect: Rect) {
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.raised,
        None,
        &RoundedRect::from_rect(rect, 18.0),
    );
}

/// The menu a box stands beside.
fn menu_panel(scene: &mut Scene, theme: &Theme, rect: Rect) {
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.overlay_subtle,
        None,
        &RoundedRect::from_rect(rect, 12.0),
    );
}
