//! Review frames of the window bar's hover intent, drawn on a GPU adapter.

use super::tests::{at_time, four_tab_bar, lay_out_at_width_with, layout_at, path, sample_at};
use super::*;

/// Before/after frames for review: the pointer resting on the ×, the pointer travelling from
/// the × to a revealed chip, and the pointer leaving for the window.
#[test]
#[ignore = "a review tool: needs a GPU adapter; writes PNGs to TILO_UI_SCREENSHOT_DIR"]
fn render_window_controls_intent_frames() {
    let dir = std::path::PathBuf::from(
        std::env::var("TILO_UI_SCREENSHOT_DIR").expect("TILO_UI_SCREENSHOT_DIR"),
    );
    std::fs::create_dir_all(&dir).expect("shot dir");
    let theme = Theme::dark().with_reduce_motion(true);
    let (w, h, scale) = (480.0, TAB_BAR_HEIGHT, 2.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "small fixed canvas"
    )]
    let (pw, ph) = ((w * scale) as u32, (h * scale) as u32);
    let mut renderer = crate::offscreen::OffscreenRenderer::try_new(pw, ph).expect("a GPU adapter");
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut shoot = |b: &mut TabBar, p: Point| -> Vec<u8> {
        let mut scene = Scene::new();
        let t = Affine::scale(scale);
        scene.fill(
            vello::peniko::Fill::NonZero,
            t,
            theme.tokens.colors.surfaces.base,
            None,
            &Rect::new(0.0, 0.0, w, h),
        );
        b.draw(&mut scene, t, &mut tr, &theme, &presence);
        let dot = vello::kurbo::Circle::new(p, 3.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            t,
            vello::peniko::Color::from_rgb8(255, 210, 0),
            None,
            &dot,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            t,
            vello::peniko::Color::BLACK,
            None,
            &dot,
        );
        renderer.render(&scene, vello::peniko::Color::BLACK)
    };
    let sheet = |name: &str, frames: Vec<(String, Vec<u8>)>| {
        let mut all = Vec::new();
        for (label, rgba) in &frames {
            let path = dir.join(format!("{name}-{label}.png"));
            crate::png::write_rgba8(&path, pw, ph, rgba).expect("write frame");
            all.extend_from_slice(rgba);
        }
        let rows = u32::try_from(frames.len()).unwrap();
        crate::png::write_rgba8(dir.join(format!("{name}.png")), pw, ph * rows, &all)
            .expect("write sheet");
    };

    // 1. Across the title onto the active pill's ×, then rest there.
    {
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        shoot(&mut b, Point::ZERO);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let r = b.tab_rects()[0];
        let x = b.close_hit_rect(0).expect("×").center();
        let start = Point::new(r.x0 + 12.0, r.center().y);
        let t0 = crate::time::now();
        let mut frames = Vec::new();
        sample_at(&mut b, t0, 0, start, &theme);
        frames.push(("00-enter".to_owned(), shoot(&mut b, start)));
        let mut ms = 0;
        for p in path(start, x, 10) {
            ms += 12;
            sample_at(&mut b, t0, ms, p, &theme);
        }
        frames.push(("01-on-x".to_owned(), shoot(&mut b, x)));
        for later in [100, 200, 400, 1_000] {
            layout_at(&mut b, t0, ms + later, &theme);
            let at = t0 + Duration::from_millis(ms + later);
            frames.push((
                format!("02-rest-{later:04}ms"),
                at_time(at, || shoot(&mut b, x)),
            ));
        }
        sheet("rest-on-x", frames);
    }

    // 2. Pill 1 opened from its body; from its × up over the bar and
    //    onto search.
    {
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        shoot(&mut b, Point::ZERO);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let r = b.tab_rects()[1];
        let t0 = crate::time::now();
        let start = Point::new(r.x0 + 12.0, r.center().y);
        let mut frames = Vec::new();
        sample_at(&mut b, t0, 0, start, &theme);
        frames.push(("00-enter-body".to_owned(), shoot(&mut b, start)));
        layout_at(&mut b, t0, 400, &theme);
        frames.push(("01-rest-400ms".to_owned(), shoot(&mut b, start)));
        let x = if b.close_rects[1].area() > 0.0 {
            b.close_rects[1].center()
        } else {
            b.tab_rects()[1].center()
        };
        let mut ms = 400;
        for p in path(start, x, 6) {
            ms += 12;
            sample_at(&mut b, t0, ms, p, &theme);
        }
        frames.push(("02-on-x".to_owned(), shoot(&mut b, x)));
        let chip = if b.search_rects()[1].area() > 0.0 {
            b.search_rects()[1].center()
        } else {
            Point::new(x.x - 48.0, x.y)
        };
        let lift = Point::new((x.x + chip.x) / 2.0, b.rect.y0 + 2.0);
        for (k, p) in path(x, lift, 3)
            .into_iter()
            .chain(path(lift, chip, 3))
            .enumerate()
        {
            ms += 12;
            sample_at(&mut b, t0, ms, p, &theme);
            frames.push((format!("03-aim-{k}"), shoot(&mut b, p)));
        }
        // 3. Off toward the window.
        let away = Point::new(chip.x, 300.0);
        ms += 12;
        sample_at(&mut b, t0, ms, Point::new(chip.x, h - 1.0), &theme);
        ms += 12;
        sample_at(&mut b, t0, ms, away, &theme);
        frames.push((
            "04-leave".to_owned(),
            shoot(&mut b, Point::new(chip.x, h - 1.0)),
        ));
        sheet("aim-from-x", frames);
    }

    // 3. Onto the edge of the ×'s 32 px target (a press there closes the
    //    tab), outside the painted glyph, and rest.
    {
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        shoot(&mut b, Point::ZERO);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let r = b.tab_rects()[0];
        let target = b.close_hit_rect(0).expect("×");
        let edge = Point::new(target.x0 + 3.0, target.y0 + 4.0);
        let t0 = crate::time::now();
        let mut frames = Vec::new();
        sample_at(&mut b, t0, 0, Point::new(edge.x, r.y0 - 20.0), &theme);
        sample_at(&mut b, t0, 12, edge, &theme);
        frames.push(("00-on-target".to_owned(), shoot(&mut b, edge)));
        for later in [100, 200, 400, 1_000] {
            layout_at(&mut b, t0, 12 + later, &theme);
            let at = t0 + Duration::from_millis(12 + later);
            frames.push((
                format!("01-rest-{later:04}ms"),
                at_time(at, || shoot(&mut b, edge)),
            ));
        }
        sheet("rest-on-x-target-edge", frames);
    }

    // 4. Pill 1 open, pill 2 active: the pointer leaves pill 1 for pill
    //    2's ×.
    {
        let mut b = four_tab_bar();
        b.set_active(3);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        shoot(&mut b, Point::ZERO);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let r = b.tab_rects()[1];
        let t0 = crate::time::now();
        let start = Point::new(r.x0 + 12.0, r.center().y);
        let mut frames = Vec::new();
        sample_at(&mut b, t0, 0, start, &theme);
        layout_at(&mut b, t0, 400, &theme);
        frames.push(("00-pill-1-open".to_owned(), shoot(&mut b, start)));
        let to = b.close_hit_rect(2).expect("×").center();
        let mut ms = 400;
        for (k, p) in path(start, to, 8).into_iter().enumerate() {
            ms += 12;
            sample_at(&mut b, t0, ms, p, &theme);
            if k % 2 == 1 {
                frames.push((format!("01-toward-x-{k}"), shoot(&mut b, p)));
            }
        }
        layout_at(&mut b, t0, ms + 1_000, &theme);
        let at = t0 + Duration::from_millis(ms + 1_000);
        frames.push((
            "02-rest-1000ms".to_owned(),
            at_time(at, || shoot(&mut b, to)),
        ));
        sheet("toward-neighbour-x", frames);
    }

    // 5. From the pill on the right, across the active pill's × at speed,
    //    onto where its chips will be: transit, so it opens.
    {
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        shoot(&mut b, Point::ZERO);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let y = b.tab_rects()[0].center().y;
        let close = b.close_hit_rect(0).expect("×");
        let from = Point::new(b.tab_rects()[1].x0 + 10.0, y);
        let to = Point::new(close.x0 - 30.0, y);
        let t0 = crate::time::now();
        let mut frames = Vec::new();
        sample_at(&mut b, t0, 0, from, &theme);
        frames.push(("00-start".to_owned(), shoot(&mut b, from)));
        let mut ms = 0;
        for (k, p) in path(from, to, 5).into_iter().enumerate() {
            ms += 8;
            sample_at(&mut b, t0, ms, p, &theme);
            frames.push((format!("01-cross-{k}"), shoot(&mut b, p)));
        }
        layout_at(&mut b, t0, ms + 120, &theme);
        frames.push(("02-rest-120ms".to_owned(), shoot(&mut b, to)));
        sheet("cross-x-to-chips", frames);
    }
}
