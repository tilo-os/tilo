//! A phone's window bar: what stands on it, and in which order — the strip's own "+", the app's
//! Back and Forward, the name the window goes by, and the actions beside them — so no two controls
//! share a place at any width.

use super::tests::lay_out_at_width_with;
use super::*;

tilo_test_env::scenario! {
    /// The bar at a phone's width.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_phone_bar_keeps_its_controls_and_tabs_apart;



    phase a_phone_window_of_several_tabs_keeps_each_tab_and_its_close;
    // A window of several tabs on a phone, as the shell lays one out (its search, the app's History
    // and the strip's "+" in the same row): every tab stays a tab a person can read and press, the
    // active one keeps its ×, and the window keeps its maximize and close.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = tilo_ui_base::theme::Theme::dark().with_reduce_motion(true);
        for width in [326.0, 400.0, 636.0] {
            let mut b = TabBar::new(
                "g1",
                [
                    (1, "Projects", AppKind::Editor),
                    (2, "Sheets", AppKind::App(tilo_ui_base::icons::Icon::Table)),
                    (3, "Files", AppKind::Files),
                ]
                .into_iter()
                .map(|(id, title, kind)| TabDescriptor {
                    app_kind: Some(kind),
                    ..TabDescriptor::new(id, title)
                })
                .collect(),
                1,
            );
            b.set_window_controls(true);
            b.set_search_control(Some(
                crate::widget::window_controls::CONTROL_PX
                    + crate::widget::window_controls::CONTROL_GAP,
            ));
            b.set_history_control(1, Some(HistoryControl::Closed));
            lay_out_at_width_with(&mut b, width, &theme);
            let controls = *b.window_controls();
            assert!(controls.close.area() > 0.0, "{width}: the window closes");
            assert!(
                controls.maximize.area() > 0.0,
                "{width}: the window maximizes"
            );
            for (i, pill) in b.tab_rects().iter().enumerate() {
                assert!(
                    pill.width() >= PILL_PAD_X + ICON_W,
                    "{width}: tab {i} shows at least its glyph, not a sliver: {pill:?}"
                );
            }
            assert!(
                b.close_rects[0].area() > 0.0,
                "{width}: the active tab offers its × ({:?})",
                b.tab_rects()
            );
            assert!(
                b.close_affordance_visible_for_tests(0, &theme)
                    && b.reveal_px[0] >= Reveal::Close.reserve(),
                "{width}: and paints it whole, not faded out ({:?})",
                b.reveal_px
            );
            let pills = b.tab_rects();
            for pair in pills.windows(2) {
                assert!(
                    pair[0].x1 <= pair[1].x0,
                    "{width}: tabs never overlap: {pills:?}"
                );
            }
            let last = pills.last().expect("tabs");
            let plus = b.new_tab_rect();
            assert!(
                plus.area() <= 0.0 || plus.x0 >= last.x1,
                "{width}: the + stands after the tabs"
            );
        }
    }
}
