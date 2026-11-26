//! A strip laid out the first time shows what a tab says about itself at once.

#![allow(
    clippy::unwrap_used,
    reason = "test fixture: failures are the assertion"
)]

use super::*;

/// A view's strip (no new-tab button, no maximize) laid out the first time, before any
/// pill has moved: the tab in front holds its ×, a changed one its dot, both at full strength.
#[test]
fn a_views_strip_shows_the_front_tabs_close_and_a_changed_tabs_dot_at_once() {
    let tab = |id: u64, title: &str, icon: icons::Icon| {
        let mut tab = TabDescriptor::new(id, title);
        tab.app_kind = Some(crate::widget::AppKind::App(icon));
        tab
    };
    let mut b = TabBar::new(
        "g1",
        vec![
            tab(1, "Notes", icons::Icon::FileText),
            tab(2, "Tasks", icons::Icon::ListChecks),
            tab(3, "Mail", icons::Icon::Mail),
        ],
        1,
    )
    .without_new_tab();
    b.set_show_maximize(false);
    b.set_closable(true);
    b.set_group_focused(true);
    b.set_modified(2, true);
    tests::lay_out_at_width(&mut b, 900.0);
    for (id, what) in [(1, "the tab in front"), (2, "the changed tab")] {
        let idx = b.tabs().iter().position(|t| t.id == id).unwrap();
        assert_ne!(
            b.close_rects[idx],
            Rect::ZERO,
            "{what} keeps its close part"
        );
        let zone = b.reveal_px[idx];
        assert!(
            chip_alpha_at(zone, CLOSE_HIT_PX) > 0.99,
            "{what} shows it at full strength at rest: zone {zone}"
        );
    }
    let theme = Theme::light();
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    // As a window does: the first draw measures the titles, the next layout and draw follow.
    tests::lay_out_at_width_with(&mut b, 900.0, &theme);
    Widget::draw(
        &mut b,
        &mut tilo_ui_base::draw::DrawCtx::new(
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            &mut text,
            &theme,
            tilo_ui_base::presence::PresenceCtx::default().draw_env(),
        ),
    );
    tests::lay_out_at_width_with(&mut b, 900.0, &theme);
    let mut scene = vello::Scene::new();
    Widget::draw(
        &mut b,
        &mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut text,
            &theme,
            tilo_ui_base::presence::PresenceCtx::default().draw_env(),
        ),
    );
    let rgba = tilo_ui_base::offscreen::OffscreenRenderer::new(900, 60)
        .render(&scene, vello::peniko::Color::WHITE);
    for (id, what) in [(1, "the tab in front's ×"), (2, "the changed tab's dot")] {
        let idx = b.tabs().iter().position(|t| t.id == id).unwrap();
        let at = b.close_rects[idx].center();
        let pill = b.tab_rects[idx];
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test pixels"
        )]
        let (x, y, ground_x) = (at.x as u32, at.y as u32, (pill.x0 + 3.0) as u32);
        let (r, ..) = tilo_ui_base::offscreen::OffscreenRenderer::sample(&rgba, 900, x, y);
        let (ground, ..) =
            tilo_ui_base::offscreen::OffscreenRenderer::sample(&rgba, 900, ground_x, y);
        assert!(
            i32::from(ground) - i32::from(r) > 40,
            "{what} is drawn at {at:?}: {r} on a ground of {ground}"
        );
    }
}
