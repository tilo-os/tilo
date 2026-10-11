use super::*;

use vello::kurbo::Affine;
use vello::peniko::{Color, Fill, Gradient};

const A: Rect = Rect::new(0.0, 0.0, 10.0, 10.0);
const B: Rect = Rect::new(100.0, 0.0, 110.0, 10.0);

fn fill(scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, &rect);
}

/// A section drawn the way the producer draws one: tracked under an active
/// recording. `draw` receives the section and paints it.
fn section(draw: impl FnOnce(&mut Scene)) -> (Box<Scene>, SectionParts) {
    let recording = Recording::begin();
    let mut scene = Box::new(Scene::new());
    track(&scene);
    draw(&mut scene);
    let parts = recording
        .finish()
        .of(&scene)
        .expect("a tracked, well-nested section has parts");
    (scene, parts)
}

thread_local! {
    // One renderer for every frame: fonts compare by blob identity, and the
    // same desktop draws its text from the same fonts.
    static TEXT: RefCell<crate::text_render::TextRenderer> =
        RefCell::new(crate::text_render::TextRenderer::new());
}

/// Background, a part at `A` drawn by `a`, chrome between, a part at `B`
/// with a gradient and text that never changes, then more chrome.
fn desktop(a: impl FnOnce(&mut Scene), chrome: Color) -> (Box<Scene>, SectionParts) {
    section(|scene| {
        fill(
            scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 200.0, 50.0),
            Color::BLACK,
        );
        part(scene, PartKey::new("a", 1_u64), vec![A], a);
        fill(
            scene,
            Affine::IDENTITY,
            Rect::new(50.0, 0.0, 60.0, 10.0),
            chrome,
        );
        part(scene, PartKey::new("b", 2_u64), vec![B], |scene| {
            let gradient = Gradient::new_linear((100.0, 0.0), (110.0, 0.0))
                .with_stops([Color::WHITE, Color::BLACK]);
            scene.fill(Fill::NonZero, Affine::IDENTITY, &gradient, None, &B);
            TEXT.with(|text| {
                text.borrow_mut().draw_text(
                    scene,
                    "caret",
                    12.0,
                    Color::WHITE,
                    Affine::translate((100.0, 0.0)),
                    None,
                );
            });
        });
        fill(
            scene,
            Affine::IDENTITY,
            Rect::new(60.0, 0.0, 70.0, 10.0),
            chrome,
        );
    })
}

fn caret(color: Color) -> impl FnOnce(&mut Scene) {
    move |scene| {
        fill(
            scene,
            Affine::IDENTITY,
            Rect::new(2.0, 2.0, 4.0, 8.0),
            color,
        )
    }
}

#[test]
fn an_unchanged_section_has_no_damage() {
    let (old, old_parts) = desktop(caret(Color::WHITE), Color::WHITE);
    let (new, new_parts) = desktop(caret(Color::WHITE), Color::WHITE);
    assert_eq!(damage(&old, &old_parts, &new, &new_parts), Ok(Vec::new()));
}

#[test]
fn a_change_inside_a_part_damages_only_that_part() {
    let (old, old_parts) = desktop(caret(Color::WHITE), Color::WHITE);
    let (new, new_parts) = desktop(caret(Color::BLACK), Color::WHITE);
    assert_eq!(damage(&old, &old_parts, &new, &new_parts), Ok(vec![A]));
}

#[test]
fn a_part_that_grows_leaves_later_text_gradients_and_chrome_proven_equal() {
    // More commands in the first part shift every stream offset, glyph run
    // and patch after it: the rest must still compare equal.
    let (old, old_parts) = desktop(caret(Color::WHITE), Color::WHITE);
    let (new, new_parts) = desktop(
        |scene| {
            caret(Color::WHITE)(scene);
            fill(
                scene,
                Affine::IDENTITY,
                Rect::new(5.0, 2.0, 7.0, 8.0),
                Color::WHITE,
            );
        },
        Color::WHITE,
    );
    let resources = &new.encoding().resources;
    assert!(
        !resources.glyph_runs.is_empty()
            && resources
                .patches
                .iter()
                .any(|patch| matches!(patch, Patch::Ramp { .. })),
        "the later part must carry offset-bearing resources for this proof to mean anything"
    );
    assert_eq!(damage(&old, &old_parts, &new, &new_parts), Ok(vec![A]));
}

#[test]
fn a_change_outside_every_part_is_not_proven() {
    let (old, old_parts) = desktop(caret(Color::WHITE), Color::WHITE);
    let (new, new_parts) = desktop(caret(Color::WHITE), Color::BLACK);
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Err(Unproven::Section)
    );
}

#[test]
fn what_follows_a_part_does_not_depend_on_the_parts_last_transform() {
    // The encoder skips a transform equal to the last one encoded. Without
    // the boundary forcing it, chrome drawn after the part would encode
    // differently depending on the transform the part ended with.
    let shifted = Affine::translate((1.0, 0.0));
    let frame = |inner: Affine| {
        section(move |scene| {
            part(scene, PartKey::new("a", 1_u64), vec![A], |scene| {
                fill(scene, inner, Rect::new(0.0, 0.0, 2.0, 2.0), Color::WHITE);
            });
            fill(
                scene,
                shifted,
                Rect::new(50.0, 0.0, 60.0, 10.0),
                Color::WHITE,
            );
        })
    };
    let (old, old_parts) = frame(Affine::IDENTITY);
    let (new, new_parts) = frame(shifted);
    assert_eq!(damage(&old, &old_parts, &new, &new_parts), Ok(vec![A]));
}

#[test]
fn nested_parts_damage_the_innermost_part_that_changed() {
    let frame = |color: Color| {
        section(move |scene| {
            part(
                scene,
                PartKey::new("outer", 0_u64),
                vec![B.union(A)],
                |scene| {
                    fill(scene, Affine::IDENTITY, B, Color::WHITE);
                    part(scene, PartKey::new("inner", 0_u64), vec![A], caret(color));
                    fill(scene, Affine::IDENTITY, B, Color::WHITE);
                },
            );
        })
    };
    let (old, old_parts) = frame(Color::WHITE);
    let (new, new_parts) = frame(Color::BLACK);
    assert_eq!(damage(&old, &old_parts, &new, &new_parts), Ok(vec![A]));
}

#[test]
fn a_part_that_appears_damages_its_new_bounds() {
    let frame = |shown: bool| {
        section(move |scene| {
            let bounds = if shown { vec![B] } else { Vec::new() };
            part(scene, PartKey::new("toast", 0_u64), bounds, |scene| {
                if shown {
                    fill(scene, Affine::IDENTITY, B, Color::WHITE);
                }
            });
        })
    };
    let (old, old_parts) = frame(false);
    let (new, new_parts) = frame(true);
    assert_eq!(damage(&old, &old_parts, &new, &new_parts), Ok(vec![B]));
    assert_eq!(damage(&new, &new_parts, &old, &old_parts), Ok(vec![B]));
}

#[test]
fn a_part_that_draws_while_claiming_no_bounds_is_not_proven() {
    let frame = |color: Color| {
        section(move |scene| {
            part(scene, PartKey::new("a", 0_u64), Vec::new(), caret(color));
        })
    };
    let (old, old_parts) = frame(Color::WHITE);
    let (new, new_parts) = frame(Color::BLACK);
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Err(Unproven::Unbounded)
    );
}

#[test]
fn different_parts_are_not_compared() {
    let (old, old_parts) = section(|scene| {
        part(
            scene,
            PartKey::new("a", 1_u64),
            vec![A],
            caret(Color::WHITE),
        );
    });
    let (new, new_parts) = section(|scene| {
        part(
            scene,
            PartKey::new("a", 2_u64),
            vec![A],
            caret(Color::WHITE),
        );
    });
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Err(Unproven::Structure)
    );
    let (none, none_parts) = section(|scene| caret(Color::WHITE)(scene));
    assert_eq!(
        damage(&old, &old_parts, &none, &none_parts),
        Err(Unproven::Structure)
    );
}

#[test]
fn an_untracked_scene_records_nothing_and_encodes_as_without_parts() {
    let recording = Recording::begin();
    let mut with_part = Scene::new();
    part(&mut with_part, PartKey::new("a", 0_u64), vec![A], |scene| {
        fill(scene, Affine::IDENTITY, A, Color::WHITE);
    });
    fill(&mut with_part, Affine::IDENTITY, B, Color::WHITE);
    let recorded = recording.finish();
    assert!(recorded.of(&with_part).is_none());
    let mut plain = Scene::new();
    fill(&mut plain, Affine::IDENTITY, A, Color::WHITE);
    fill(&mut plain, Affine::IDENTITY, B, Color::WHITE);
    assert!(with_part.encoding_eq(&plain));
}

#[test]
fn a_part_left_open_gives_the_section_no_parts() {
    let recording = Recording::begin();
    let mut scene = Box::new(Scene::new());
    track(&scene);
    boundary(&mut scene, Boundary::Begin(PartKey::new("a", 0_u64)));
    fill(&mut scene, Affine::IDENTITY, A, Color::WHITE);
    assert!(recording.finish().of(&scene).is_none());
}

/// An app's retained scene with a caret part, appended into a tile part of
/// a section at `offset`, as an embedded app is.
fn embedded(caret_color: Color, offset: f64) -> (Box<Scene>, SectionParts) {
    let mut app = Scene::new();
    let ((), app_parts) = record(&mut app, |scene| {
        fill(
            scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 40.0, 40.0),
            Color::BLACK,
        );
        part(
            scene,
            PartKey::new("caret", 0_u64),
            vec![A],
            caret(caret_color),
        );
        fill(
            scene,
            Affine::IDENTITY,
            Rect::new(20.0, 0.0, 30.0, 10.0),
            Color::WHITE,
        );
    });
    let app_parts = app_parts.expect("the app scene's parts");
    let tile = Rect::new(offset, 0.0, offset + 40.0, 40.0);
    section(|scene| {
        fill(
            scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 200.0, 50.0),
            Color::BLACK,
        );
        part(scene, PartKey::new("tile", 0_u64), vec![tile], |scene| {
            let at = Affine::translate((offset, 0.0));
            append(scene, &app, Some(&app_parts), Some(at));
        });
        fill(scene, Affine::IDENTITY, B, Color::WHITE);
    })
}

#[test]
fn an_appended_scene_keeps_its_parts_where_it_is_drawn() {
    let (old, old_parts) = embedded(Color::WHITE, 100.0);
    let (new, new_parts) = embedded(Color::BLACK, 100.0);
    let caret = Rect::new(100.0, 0.0, 110.0, 10.0);
    assert_eq!(damage(&old, &old_parts, &new, &new_parts), Ok(vec![caret]));
}

#[test]
fn an_appended_scene_that_moves_damages_where_it_was_and_is() {
    let (old, old_parts) = embedded(Color::WHITE, 100.0);
    let (new, new_parts) = embedded(Color::WHITE, 120.0);
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Ok(vec![
            Rect::new(100.0, 0.0, 140.0, 40.0),
            Rect::new(120.0, 0.0, 160.0, 40.0),
            Rect::new(100.0, 0.0, 110.0, 10.0),
            Rect::new(120.0, 0.0, 130.0, 10.0),
        ])
    );
}

#[test]
fn appending_into_an_untracked_scene_is_a_plain_append() {
    let mut app = Scene::new();
    let ((), parts) = record(&mut app, |scene| {
        part(
            scene,
            PartKey::new("caret", 0_u64),
            vec![A],
            caret(Color::WHITE),
        );
    });
    let mut appended = Scene::new();
    append(&mut appended, &app, parts.as_ref(), None);
    let mut plain = Scene::new();
    plain.append(&app, None);
    assert!(appended.encoding_eq(&plain));
}

/// An overlay part that is always there, opening a menu with a part of its
/// own inside: the parts nested in it differ between the frames.
fn overlay(open: bool, chrome: Color) -> (Box<Scene>, SectionParts) {
    section(move |scene| {
        fill(
            scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 200.0, 50.0),
            chrome,
        );
        let bounds = if open { vec![B] } else { Vec::new() };
        part(scene, PartKey::new("menu", 0_u64), bounds, |scene| {
            if open {
                fill(scene, Affine::IDENTITY, B, Color::WHITE);
                part(scene, PartKey::new("row", 0_u64), vec![B], |scene| {
                    fill(scene, Affine::IDENTITY, B, Color::BLACK);
                });
            }
        });
        part(
            scene,
            PartKey::new("caret", 0_u64),
            vec![A],
            caret(Color::WHITE),
        );
    })
}

#[test]
fn a_part_whose_nested_parts_change_damages_its_bounds_as_a_whole() {
    let (closed, closed_parts) = overlay(false, Color::BLACK);
    let (open, open_parts) = overlay(true, Color::BLACK);
    assert_eq!(
        damage(&closed, &closed_parts, &open, &open_parts),
        Ok(vec![B])
    );
    assert_eq!(
        damage(&open, &open_parts, &closed, &closed_parts),
        Ok(vec![B])
    );
}

#[test]
fn comparing_outermost_parts_still_refuses_a_change_outside_them() {
    let (closed, closed_parts) = overlay(false, Color::BLACK);
    let (open, open_parts) = overlay(true, Color::WHITE);
    assert_eq!(
        damage(&closed, &closed_parts, &open, &open_parts),
        Err(Unproven::Section)
    );
}

/// An embedded app's content placed at `offset` inside a tile part, its
/// caret painted in `color`, with a hint from `source` that generation
/// `from` → `to` changed the caret.
fn hinted_app(
    color: Color,
    offset: f64,
    hint: Option<(u64, u64, u64)>,
) -> (Box<Scene>, SectionParts) {
    let tile = Rect::new(offset, 0.0, offset + 40.0, 40.0);
    let placement = Affine::translate((offset, 0.0));
    section(move |scene| {
        part(scene, PartKey::new("tile", 0_u64), vec![tile], |scene| {
            let hint = hint.map(|(source, from, to)| DamageHint {
                source,
                from,
                to,
                placement,
                rects: vec![placement.transform_rect_bbox(A)],
                copies: Vec::new(),
            });
            hinted(scene, PartKey::new("app", 0_u64), hint, |scene| {
                fill(
                    scene,
                    placement,
                    Rect::new(0.0, 0.0, 40.0, 40.0),
                    Color::BLACK,
                );
                fill(scene, placement, Rect::new(2.0, 2.0, 4.0, 8.0), color);
            });
        });
    })
}

#[test]
fn a_hint_that_follows_on_damages_what_it_says_changed() {
    let (old, old_parts) = hinted_app(Color::WHITE, 100.0, Some((7, 0, 1)));
    let (new, new_parts) = hinted_app(Color::BLACK, 100.0, Some((7, 1, 2)));
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Ok(vec![Rect::new(100.0, 0.0, 110.0, 10.0)])
    );
}

#[test]
fn a_hint_that_does_not_follow_on_damages_the_enclosing_part() {
    let tile = Rect::new(100.0, 0.0, 140.0, 40.0);
    let (old, old_parts) = hinted_app(Color::WHITE, 100.0, Some((7, 0, 1)));
    // A generation was skipped, another source painted, or there is no hint.
    for hint in [Some((7, 2, 3)), Some((8, 1, 2)), None] {
        let (new, new_parts) = hinted_app(Color::BLACK, 100.0, hint);
        assert_eq!(
            damage(&old, &old_parts, &new, &new_parts),
            Ok(vec![tile]),
            "{hint:?}"
        );
    }
}

#[test]
fn a_hint_for_content_that_moved_damages_where_it_was_and_is() {
    let (old, old_parts) = hinted_app(Color::WHITE, 100.0, Some((7, 0, 1)));
    let (new, new_parts) = hinted_app(Color::WHITE, 120.0, Some((7, 1, 2)));
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Ok(vec![
            Rect::new(100.0, 0.0, 140.0, 40.0),
            Rect::new(120.0, 0.0, 160.0, 40.0)
        ])
    );
}

#[test]
fn a_hinted_part_outside_every_bounded_part_is_not_proven_without_its_hint() {
    let hint = |from: u64, to: u64| DamageHint {
        source: 1,
        from,
        to,
        placement: Affine::IDENTITY,
        rects: vec![A],
        copies: Vec::new(),
    };
    let frame = |color: Color, hint: Option<DamageHint>| {
        section(move |scene| {
            hinted(scene, PartKey::new("app", 0_u64), hint, caret(color));
        })
    };
    let (old, old_parts) = frame(Color::WHITE, Some(hint(0, 1)));
    let (new, new_parts) = frame(Color::BLACK, Some(hint(1, 2)));
    assert_eq!(damage(&old, &old_parts, &new, &new_parts), Ok(vec![A]));
    let (new, new_parts) = frame(Color::BLACK, None);
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Err(Unproven::Section)
    );
    // A hint that says nothing changed cannot vouch for a change.
    let mut empty = hint(1, 2);
    empty.rects.clear();
    let (new, new_parts) = frame(Color::BLACK, Some(empty));
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Err(Unproven::Section)
    );
}

#[test]
fn an_appended_hint_is_carried_to_where_the_scene_is_drawn() {
    let frame = |color: Color, from: u64, to: u64| {
        let mut app = Scene::new();
        let ((), parts) = record(&mut app, |scene| {
            let hint = DamageHint {
                source: 3,
                from,
                to,
                placement: Affine::IDENTITY,
                rects: vec![A],
                copies: Vec::new(),
            };
            hinted(scene, PartKey::new("app", 0_u64), Some(hint), caret(color));
        });
        let parts = parts.expect("the app scene's parts");
        section(move |scene| {
            part(scene, PartKey::new("tile", 0_u64), vec![B], |scene| {
                let at = Affine::translate((100.0, 0.0));
                append(scene, &app, Some(&parts), Some(at));
            });
        })
    };
    let (old, old_parts) = frame(Color::WHITE, 0, 1);
    let (new, new_parts) = frame(Color::BLACK, 1, 2);
    assert_eq!(
        damage(&old, &old_parts, &new, &new_parts),
        Ok(vec![Rect::new(100.0, 0.0, 110.0, 10.0)])
    );
}
