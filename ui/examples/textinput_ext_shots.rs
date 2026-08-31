//! Offscreen shots for the two `TextInput` capabilities added for the
//! hand-rolled-UI migration: the **multiline** mode (soft-wrapped rows +
//! cross-line selection + caret) and the **secret** mode (masked bullets, with
//! a revealed variant for contrast).
//!
//! Every widget renders through its real five-pass pipeline; the selection and
//! caret come from the production `draw` path, not private pokes (only
//! `set_selection`, a public API, arms the highlight for the still frame).
//!
//! Run:  `cargo run -p ui --example ui-shots -- textinput_ext_shots`
//! Out:  `$TEXTINPUT_EXT_SHOTS_OUT` (default `target/textinput_ext_shots`)

use std::path::PathBuf;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

fn label(scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, s: &str, x: f64, y: f64) {
    tr.draw_text_styled(
        scene,
        s,
        13.0,
        theme.text_dim,
        Affine::translate((x, y)),
        None,
        TextWeight::Semibold,
        false,
        false,
    );
}

pub fn run(_args: &[String]) {
    let dir = std::env::var("TEXTINPUT_EXT_SHOTS_OUT")
        .unwrap_or_else(|_| "target/textinput_ext_shots".to_string());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("textinput_ext_shots: failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);

    let theme = Theme::dark();
    let presence = PresenceCtx::default();
    let mut tr = TextRenderer::new();

    // ---- 1. Multiline field: wrapped paragraph + a cross-line selection ----
    let body = "The quick brown fox jumps over the lazy dog, then keeps running \
                across the whole field.\nA second paragraph starts on its own line \
                and also soft-wraps as it grows.";
    let mut ml = TextInput::local().multiline(true).with_initial_value(body);
    ml.set_focus(true);
    ml.apply_layout_external(Rect::new(30.0, 44.0, 470.0, 200.0));
    // A first draw builds the wrapped visual-line layout the selection reads.
    {
        let mut s = Scene::new();
        ml.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }
    // Select from the first line through into the second — spans a wrap and the
    // hard newline, so the highlight paints as a stack of per-row bands.
    ml.set_selection(16, 96);

    let mut scene = Scene::new();
    label(
        &mut scene,
        &mut tr,
        &theme,
        "Multiline TextInput — soft-wrap + cross-line selection",
        30.0,
        20.0,
    );
    ml.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let mut r = OffscreenRenderer::new(500, 230);
    let out_ml = dir.join("multiline.png");
    r.render_png(&scene, theme.bg_app, &out_ml);
    println!("wrote {}", out_ml.display());

    // ---- 2. Secret field: masked, and a revealed variant ------------------
    let mut masked = TextInput::local()
        .with_secret(true)
        .with_initial_value("hunter2-s3cret");
    masked.set_focus(true);
    masked.apply_layout_external(Rect::new(30.0, 52.0, 470.0, 92.0));

    let mut revealed = TextInput::local()
        .with_secret(true)
        .with_reveal(true)
        .with_initial_value("hunter2-s3cret");
    revealed.apply_layout_external(Rect::new(30.0, 140.0, 470.0, 180.0));

    let mut scene2 = Scene::new();
    label(
        &mut scene2,
        &mut tr,
        &theme,
        "Secret TextInput — masked (focused, caret)",
        30.0,
        30.0,
    );
    masked.draw(&mut scene2, Affine::IDENTITY, &mut tr, &theme, &presence);
    label(
        &mut scene2,
        &mut tr,
        &theme,
        "Secret TextInput — revealed (eye toggle on)",
        30.0,
        118.0,
    );
    revealed.draw(&mut scene2, Affine::IDENTITY, &mut tr, &theme, &presence);

    let mut r2 = OffscreenRenderer::new(500, 210);
    let out_secret = dir.join("secret.png");
    r2.render_png(&scene2, theme.bg_app, &out_secret);
    println!("wrote {}", out_secret.display());

    // Sanity: the still frames reflect real widget state.
    assert!(ml.is_multiline());
    assert_eq!(
        ml.selection_range_public(),
        Some((16, 96)),
        "cross-line selection armed"
    );
    assert!(
        masked.is_secret() && !masked.is_revealed(),
        "masked field stays masked"
    );
    assert_eq!(
        masked.value(),
        "hunter2-s3cret",
        "model holds the real secret under the mask"
    );
    assert!(revealed.is_revealed(), "revealed field shows its glyphs");
    println!("OK: multiline (wrap+selection) + secret (masked/revealed) rendered");
}
