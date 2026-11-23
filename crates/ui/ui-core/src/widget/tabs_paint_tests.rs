//! A bar is drawn once at rest and again only on a change.

use super::super::{BUSY_TURNING, TabBar, TabDescriptor, TabMark, tab_presence_key};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::testing::with_monotonic_time;
use crate::widget::Widget;
use vello::Scene;
use vello::kurbo::Affine;

fn bar() -> TabBar {
    let mut bar = TabBar::new(
        "g1",
        vec![
            TabDescriptor::new(1, "A"),
            TabDescriptor::new(2, "B"),
            TabDescriptor::new(3, "C"),
        ],
        1,
    );
    bar.force_settled_for_tests();
    super::super::tests::lay_out_at_width_with(&mut bar, 900.0, &Theme::dark());
    bar
}

/// One draw of `bar` at rest, as a host paints it.
fn painted(bar: &mut TabBar, tr: &mut TextRenderer, theme: &Theme) -> Scene {
    let mut scene = Scene::new();
    bar.draw(&mut crate::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        tr,
        theme,
        PresenceCtx::default().draw_env(),
    ));
    scene
}

tilo_test_env::scenario! {
    /// When the bar repaints and asks for a frame.
    /// Each phase was a test of its own and builds its own fixture.
    fn the_bar_is_drawn_and_asks_for_frames_only_on_a_change;

    phase a_bar_at_rest_is_drawn_once_and_again_only_on_a_change;
    // A bar at rest is drawn once and shown again on the frames after,
    // exactly as drawn; a change of its own, its theme, or the people on
    // its tabs draws it afresh.
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut bar = bar();
        let first = painted(&mut bar, &mut tr, &theme);
        let drawn = bar.fresh_draws;
        for _ in 0..10 {
            let again = painted(&mut bar, &mut tr, &theme);
            assert!(again.encoding_eq(&first), "shown as it was drawn");
        }
        assert_eq!(bar.fresh_draws, drawn, "frames at rest drew the bar again");
        bar.set_modified(2, true);
        let _ = painted(&mut bar, &mut tr, &theme);
        assert_eq!(bar.fresh_draws, drawn + 1, "a change of its own");
        let _ = painted(&mut bar, &mut tr, &Theme::light());
        assert_eq!(bar.fresh_draws, drawn + 2, "another theme");
        let mut presence = PresenceCtx::default();
        presence.register_peer_at(::collaboration::permissions::MemberId::person("peer"), &tab_presence_key(2));
        let mut scene = Scene::new();
        bar.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &Theme::light(), presence.draw_env()));
        assert_eq!(bar.fresh_draws, drawn + 3, "someone came to a tab");
    }

    phase a_drawn_bar_asks_for_a_frame_only_on_a_change_or_a_person;
    // A bar that drew is clean: it asks for no frame until its own state moves
    // or the people on its tabs are no longer the people it drew — both read
    // before a draw, from the presence the host lends the pass.
    {
        use crate::presence::frame;
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut bar = bar();
        let mut presence = PresenceCtx::default();
        let clean =
            |bar: &TabBar, presence: &PresenceCtx| frame::lend(presence, || !Widget::needs_redraw(bar));
        let draw = |bar: &mut TabBar, tr: &mut TextRenderer, presence: &PresenceCtx| {
            frame::lend(presence, || {
                let mut scene = Scene::new();
                bar.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, tr, &theme, presence.draw_env()));
            });
        };
        assert!(!clean(&bar, &presence), "an undrawn bar repaints");
        draw(&mut bar, &mut tr, &presence);
        assert!(clean(&bar, &presence), "a bar that drew is clean");
        presence.register_peer_at(::collaboration::permissions::MemberId::person("peer"), &tab_presence_key(2));
        assert!(!clean(&bar, &presence), "someone came to a tab");
        draw(&mut bar, &mut tr, &presence);
        assert!(clean(&bar, &presence), "and is clean drawn with them");
        bar.set_modified(2, true);
        assert!(!clean(&bar, &presence), "a change of its own");
    }

    phase a_turning_busy_arc_repaints_only_its_square;
    // A turning busy arc draws its bar on every frame, in a damage part of
    // its own, so only its square repaints; at rest, the bar is shown again.
    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut bar = bar();
        bar.set_mark(2, Some(TabMark::Busy));
        let since = bar.busy_since[&2];
        let frame = |bar: &mut TabBar, tr: &mut TextRenderer, at| {
            with_monotonic_time(at, || {
                let mut scene = Scene::new();
                let recording = crate::scene_parts::Recording::begin();
                crate::scene_parts::track(&scene);
                bar.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, tr, &theme, PresenceCtx::default().draw_env()));
                let parts = recording.finish().of(&scene).expect("well nested parts");
                (scene, parts)
            })
        };
        let step = std::time::Duration::from_millis(40);
        let (before, before_parts) = frame(&mut bar, &mut tr, since + step);
        let drawn = bar.fresh_draws;
        let (after, after_parts) = frame(&mut bar, &mut tr, since + step * 2);
        assert_eq!(
            bar.fresh_draws,
            drawn + 1,
            "a turning arc draws every frame"
        );
        let damage = crate::scene_parts::damage(&before, &before_parts, &after, &after_parts)
            .expect("the frames compare part by part");
        let square = bar.tab_rects[1];
        assert!(!damage.is_empty(), "the arc moved");
        for rect in &damage {
            assert!(
                rect.width() <= 24.0 && rect.height() <= 24.0,
                "{rect:?} is more than the arc's square in {square:?}"
            );
        }
        let rested = since + BUSY_TURNING + step;
        let _ = frame(&mut bar, &mut tr, rested);
        let drawn = bar.fresh_draws;
        let _ = frame(&mut bar, &mut tr, rested + step);
        assert_eq!(bar.fresh_draws, drawn, "a still ring is shown again");
    }

    phase every_change_to_the_bar_asks_for_a_frame;
    // Every change of the bar's own state marks what it drew as stale, so a
    // frame never shows a drawing of the bar from before the change.
    {
        use crate::presence::frame;
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let clean = |bar: &TabBar| frame::lend(&presence, || !Widget::needs_redraw(bar));
        let draw = |bar: &mut TabBar, tr: &mut TextRenderer| {
            frame::lend(&presence, || {
                let mut scene = Scene::new();
                bar.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, tr, &theme, presence.draw_env()));
            });
        };
        #[allow(clippy::type_complexity, reason = "one table of one-line changes")]
        let changes: Vec<(&str, Box<dyn Fn(&mut TabBar)>)> = vec![
            (
                "a tab marked edited",
                Box::new(|bar: &mut TabBar| bar.set_modified(2, true)),
            ),
            (
                "a tab marked busy",
                Box::new(|bar: &mut TabBar| bar.set_mark(2, Some(TabMark::Busy))),
            ),
            (
                "a tab given its history",
                Box::new(|bar: &mut TabBar| {
                    bar.set_history_control(2, Some(crate::widget::HistoryControl::Open));
                }),
            ),
            (
                "the window control's meaning",
                Box::new(|bar: &mut TabBar| {
                    bar.set_resize(crate::widget::window_controls::Resize::Restore);
                }),
            ),
            (
                "a bar made carriable",
                Box::new(|bar: &mut TabBar| bar.set_carriable(true)),
            ),
            (
                "the group focused",
                Box::new(|bar: &mut TabBar| bar.set_group_focused(true)),
            ),
            (
                "the opened-by mark focused",
                Box::new(|bar: &mut TabBar| bar.set_opened_by_focused(true)),
            ),
        ];
        for (change, mutate) in changes {
            let mut bar = bar();
            draw(&mut bar, &mut tr);
            assert!(clean(&bar), "{change}: the bar is clean once drawn");
            mutate(&mut bar);
            assert!(!clean(&bar), "{change}: a change asks for a frame");
        }
    }

    phase a_slot_change_repaints_only_the_bar;
    // The app's slots changing on their own (a document's edited mark coming
    // and going with each save) repaint the bar, not the window under it.
    {
        let _settled = crate::motion::testing::settled();
        let theme = Theme::dark().with_reduce_motion(true);
        let mut tr = TextRenderer::new();
        let mut bar = TabBar::new("g1", vec![TabDescriptor::new(1, "Notes")], 1);
        bar.set_window_controls(true);
        let slots = |edited| crate::window_toolbar::WindowToolbar {
            sidebar: Some(true),
            location: Some(crate::window_toolbar::ToolbarLocation {
                label: "Ideas".into(),
                edited,
                ..crate::window_toolbar::ToolbarLocation::default()
            }),
            ..crate::window_toolbar::WindowToolbar::default()
        };
        let mut frame = |bar: &mut TabBar, edited| {
            bar.set_toolbar(1, Some(slots(edited)));
            super::super::tests::lay_out_at_width_with(bar, 900.0, &theme);
            let mut scene = Scene::new();
            let recording = crate::scene_parts::Recording::begin();
            crate::scene_parts::track(&scene);
            bar.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, PresenceCtx::default().draw_env()));
            let parts = recording.finish().of(&scene).expect("well nested parts");
            (scene, parts)
        };
        let (saved, saved_parts) = frame(&mut bar, false);
        let (edited, edited_parts) = frame(&mut bar, true);
        assert!(!edited.encoding_eq(&saved), "the edited mark shows");
        let damage = crate::scene_parts::damage(&saved, &saved_parts, &edited, &edited_parts)
            .expect("the frames compare part by part");
        assert!(!damage.is_empty());
        let height = bar.bar_height();
        for rect in &damage {
            assert!(
                rect.height() <= height + 4.0,
                "{rect:?} is more than the bar's {height}px"
            );
        }
    }
}
