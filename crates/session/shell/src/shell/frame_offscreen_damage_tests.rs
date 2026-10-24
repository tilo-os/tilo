//! Client damage where no display shows it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "buffer index arithmetic over a fixed test allocation"
)]

use super::*;

/// Fill `rect` of an RGBA buffer `size` pixels across with `texel`.
fn fill_buffer_rect(pixels: &mut [u8], size: (u32, u32), rect: Rect, texel: [u8; 4]) {
    for y in rect.y0 as usize..rect.y1 as usize {
        for x in rect.x0 as usize..rect.x1 as usize {
            let index = (y * size.0 as usize + x) * 4;
            pixels[index..index + 4].copy_from_slice(&texel);
        }
    }
}

/// A window wider than the display (the strip scrolls, so a resize can make a
/// window any width) changes only where no display shows it: nothing on
/// screen changed, so the frame repaints nothing. Changing there and on
/// screen at once repaints only what is on screen. Damage no display showed
/// used to void the whole frame's damage, so a caret blinking in the hidden
/// part of a window, or a neighbour peeking in from the strip's edge,
/// repainted the entire output on every commit.
#[test]
fn client_damage_no_display_shows_repaints_nothing() {
    let _ambient = DenyAmbientStarts::new();
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let surface = 8_493_302;
    let (mut fixture, size) =
        foreign_damage_fixture(ui::widget::WindowWidth::Fraction(1_500), surface);
    let mut pixels = [32, 32, 32, 255].repeat(size.0 as usize * size.1 as usize);
    let store = crate::shell::foreign::store();
    store.publish_rgba(surface, &pixels, size.0, size.1);
    let _first = fixture.paint_with_damage();
    let mounted = fixture.foreign_surface_rect(surface).expect("mounted");
    let (scene_width, _) = fixture.scene_size();
    let scale_x = mounted.width() / f64::from(size.0);
    // Twenty buffer columns that land two tiles clear of the display's edge,
    // on whichever side of it the window reaches past.
    let margin = 32.0;
    let hidden_x0 = if mounted.x1 > scene_width + margin * 2.0 {
        ((scene_width + margin - mounted.x0) / scale_x).ceil()
    } else {
        assert!(
            mounted.x0 < -margin * 2.0,
            "test setup: the window {mounted:?} must reach past the {scene_width}-pixel display"
        );
        ((-margin - mounted.x0) / scale_x).floor() - 20.0
    };
    assert!(
        hidden_x0 >= 0.0 && hidden_x0 + 20.0 <= f64::from(size.0),
        "test setup: hidden columns {hidden_x0} must lie in the {size:?} buffer"
    );
    let hidden = Rect::new(hidden_x0, 100.0, hidden_x0 + 20.0, 120.0);
    fill_buffer_rect(&mut pixels, size, hidden, [224, 224, 224, 255]);
    store.publish_damaged_rgba_for_test(surface, &pixels, size, vec![hidden]);
    let unseen = fixture.paint_with_damage();
    assert_eq!(
        unseen.damage_regions.as_deref(),
        Some(&[][..]),
        "a change no display shows repaints nothing, not the whole output"
    );

    // A visible line changes in the same commit as the hidden one.
    let visible_x0 = ((mounted.x0.max(0.0) + 256.0 - mounted.x0) / scale_x).ceil();
    let visible = Rect::new(visible_x0, 300.0, visible_x0 + 200.0, 320.0);
    fill_buffer_rect(&mut pixels, size, hidden, [96, 96, 96, 255]);
    fill_buffer_rect(&mut pixels, size, visible, [224, 224, 224, 255]);
    store.publish_damaged_rgba_for_test(surface, &pixels, size, vec![hidden, visible]);
    let both = fixture.paint_with_damage();
    let damage = both
        .damage_regions
        .as_ref()
        .expect("a change partly on screen keeps bounded damage");
    let scale_y = mounted.height() / f64::from(size.1);
    let shown = Rect::new(
        mounted.x0 + visible.x0 * scale_x,
        mounted.y0 + visible.y0 * scale_y,
        mounted.x0 + visible.x1 * scale_x,
        mounted.y0 + visible.y1 * scale_y,
    );
    assert_damage_covers(shown, damage, "the visible line");
    let damaged: u64 = damage
        .iter()
        .map(|rect| u64::from(rect.width) * u64::from(rect.height))
        .sum();
    // The line, one texel of filtering around it, rounded out to whole tiles.
    let tiles = ((shown.width() + 2.0) / 16.0).ceil() + 1.0;
    let rows = ((shown.height() + 2.0) / 16.0).ceil() + 1.0;
    assert!(
        damaged <= (tiles * rows * 256.0) as u64,
        "{damaged} damaged pixels for a {shown:?} line: {damage:?}"
    );
    store.forget(surface);
}
