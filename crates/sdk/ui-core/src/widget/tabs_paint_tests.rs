//! A bar is drawn once at rest and again only on a change.

#![allow(
    clippy::unwrap_used,
    reason = "test fixture: failures are the assertion"
)]

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
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        tr,
        theme,
        &PresenceCtx::default(),
    );
    scene
}

/// A bar at rest is drawn once and shown again on the frames after,
/// exactly as drawn; a change of its own, its theme, or the people on
/// its tabs draws it afresh.
#[test]
fn a_bar_at_rest_is_drawn_once_and_again_only_on_a_change() {
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
    presence.register_peer_at("peer".to_string(), tab_presence_key(2));
    let mut scene = Scene::new();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &Theme::light(),
        &presence,
    );
    assert_eq!(bar.fresh_draws, drawn + 3, "someone came to a tab");
}

/// A turning busy arc draws its bar on every frame, in a damage part of
/// its own, so only its square repaints; at rest, the bar is shown again.
#[test]
fn a_turning_busy_arc_repaints_only_its_square() {
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
            bar.draw(
                &mut scene,
                Affine::IDENTITY,
                tr,
                &theme,
                &PresenceCtx::default(),
            );
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

/// Every method that can change what the bar draws says so
/// ([`TabBar::touch`]) before anything else, so a frame never shows a
/// drawing of the bar from before the change. Those that only hand the
/// host what the bar asked for, and the draw and update that judge
/// themselves, are named here.
#[test]
fn every_mutable_method_of_the_bar_touches_its_drawing() {
    const JUDGE_THEMSELVES: &[&str] = &[
        "draw",
        "draw_fresh",
        "draw_overlay",
        "touch",
        "update",
        "settle_hover_intent",
        "cancel_close",
        "set_history_control",
        "set_maximized",
        "set_carriable",
        "set_group_focused",
        "set_popup_area",
        "set_dev_build",
        "set_desktop_control",
        "set_shared",
        "set_shared_by",
        "set_share_mark",
        "draw_shared_chip",
    ];
    let files = [
        ("tabs.rs", include_str!("tabs.rs")),
        ("tabs_rename.rs", include_str!("tabs_rename.rs")),
        ("tabs_status_chips.rs", include_str!("tabs_status_chips.rs")),
        ("tabs_title_menu.rs", include_str!("tabs_title_menu.rs")),
    ];
    let mut missing = Vec::new();
    let mut checked = 0;
    for (file, source) in files {
        let lines: Vec<&str> = source.lines().collect();
        let mut bar_impl = false;
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            if line.starts_with("impl") {
                bar_impl = line.contains("TabBar {") || line.ends_with("for TabBar {");
            } else if line.starts_with('}') {
                bar_impl = false;
            }
            let signature = line.trim_start();
            let method = line.starts_with("    fn ")
                || line.starts_with("    pub fn ")
                || line.starts_with("    pub(crate) fn ")
                || line.starts_with("    pub(super) fn ");
            if bar_impl && method {
                let name = signature
                    .split("fn ")
                    .nth(1)
                    .and_then(|rest| rest.split(['(', '<']).next())
                    .unwrap_or_default();
                let mut end = index;
                let mut whole = String::from(line);
                while !lines[end].ends_with('{') && !lines[end].ends_with(';') {
                    end += 1;
                    whole.push_str(lines[end]);
                }
                let exempt = JUDGE_THEMSELVES.contains(&name)
                    || name.starts_with("take_")
                    || name.ends_with("_for_tests");
                if whole.contains("&mut self") && lines[end].ends_with('{') && !exempt {
                    checked += 1;
                    if lines.get(end + 1).map(|line| line.trim()) != Some("self.touch();") {
                        missing.push(format!("{file}:{} {name}", index + 1));
                    }
                }
                index = end;
            }
            index += 1;
        }
    }
    assert!(checked > 50, "the scan found the methods: {checked}");
    assert!(
        missing.is_empty(),
        "methods that change the bar unseen: {missing:?}"
    );
}

/// The app's slots changing on their own (a document's edited mark coming
/// and going with each save) repaint the bar, not the window under it.
#[test]
fn a_slot_change_repaints_only_the_bar() {
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
        bar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
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
