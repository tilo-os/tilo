//! A segmented strip too narrow for its labels folds into a picker whose
//! menu chooses every segment.

use super::super::strip_menu::StripHost;
use super::*;

/// A renderer with the fonts the editor ships, so labels measure as they
/// do on screen.
fn shipped_text() -> TextRenderer {
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        ui::font_catalog::checkout_root(),
        "en-US",
    );
    TextRenderer::with_font_catalog(&catalog).expect("the shipped font fixture loads")
}

/// One frame, laid out and painted in the shipped fonts.
fn render_shipped(app: &mut VideoEditorApp) {
    let mut scene = Scene::new();
    let mut tr = shipped_text();
    app.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
}

#[test]
fn at_420_px_the_transition_strip_folds_and_every_transition_can_be_chosen() {
    let (mut app, mut doc) = app_sized(420.0, 800.0);
    *app.hit_text.borrow_mut() = Some(shipped_text());
    app.navigation.flap_settled = true;
    let clip = app
        .project
        .clips
        .iter()
        .find(|clip| {
            app.project
                .track(clip.track)
                .is_some_and(|track| track.kind == TrackKind::Video)
        })
        .map(|clip| clip.id)
        .expect("the sample has a video clip");
    app.selection.select_only(clip);
    render_shipped(&mut app);
    // The window bar's Inspector action shows it over the editor.
    app.run_window_bar_event(&ui::window_toolbar::ToolbarEvent::Action {
        id: super::super::window_bar::INSPECTOR.into(),
    });
    app.inspector_flap.snap(true);
    render_shipped(&mut app);
    let r = app.regions();
    assert!(
        r.inspector_overlay && app.inspector_flap.is_open(app.inspector_split()),
        "a 420 px window shows the inspector over the editor"
    );

    // Bring the transition row into the inspector's view.
    let mut l = app.inspector_layout(&r);
    let first = l
        .transitions
        .first()
        .map(|(_, rect)| *rect)
        .expect("a transition row");
    let view = app.inspector_viewport(&r);
    if !app.inspector_shown_body(&r, &l).contains(first.center()) {
        app.inspector_scroll += first.y0 - view.y0 - 24.0;
        render_shipped(&mut app);
        l = app.inspector_layout(&r);
    }
    let strip = l
        .transitions
        .iter()
        .fold(l.transitions[0].1, |strip, (_, rect)| strip.union(*rect));
    assert!(app.inspector_shown_body(&r, &l).contains(strip.center()));
    assert!(
        app.strips.is_folded(StripHost::Inspector, strip),
        "five transitions do not fit whole in a {:.0} px strip: it folds",
        strip.width()
    );

    for transition in Transition::ALL {
        click(&mut app, &mut doc, strip.center().x, strip.center().y);
        release(&mut app, &mut doc);
        assert!(
            app.bin_menu.is_open(),
            "a press on the folded strip opens its menu"
        );
        let rows: Vec<&str> = app
            .bin_menu
            .items()
            .iter()
            .filter_map(|i| i.label())
            .collect();
        let labels: Vec<&str> = Transition::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(rows, labels, "the menu lists every transition, whole");
        click_menu_item(&mut app, &mut doc, transition.label());
        release(&mut app, &mut doc);
        sync(&mut app, &mut doc);
        render_shipped(&mut app);
        assert_eq!(
            app.project.clip(clip).map(|c| c.transition),
            Some(transition),
            "{} is chosen from the menu",
            transition.label()
        );
        assert!(!app.bin_menu.is_open(), "choosing closes the menu");
    }
}
