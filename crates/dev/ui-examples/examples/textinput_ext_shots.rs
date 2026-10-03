//! Offscreen shots for `TextInput`'s **multiline** mode (soft-wrapped rows,
//! cross-line selection, and caret) and **secret** mode (masked bullets, with
//! a revealed variant for contrast).
//!
//! The shots use the normal five-pass pipeline. `set_selection` only prepares
//! the public selection state for the still frame.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- textinput_ext_shots`
//! Out:  `$TEXTINPUT_EXT_SHOTS_OUT` (default `target/textinput_ext_shots`)

use std::path::PathBuf;

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};

use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::{TextRenderer, TextStyle, TextWeight};
use tilo_ui::theme::Theme;
use tilo_ui::widget::{TextInput, TextInputKind, Widget};

fn label(scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, s: &str, x: f64, y: f64) {
    tr.draw_style(
        scene,
        s,
        theme.text_dim,
        Affine::translate((x, y)),
        None,
        &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "the scene setup follows its frame-construction order"
)]
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
    r.render_png(&scene, theme.tokens.colors.surfaces.content, &out_ml);
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
    r2.render_png(&scene2, theme.tokens.colors.surfaces.content, &out_secret);
    println!("wrote {}", out_secret.display());

    // ---- 3. Writing assistance: spelling + extensible text actions -------
    let copy = "Please chek teh release notes before sharing them with the team.";
    let mut assisted = TextInput::local()
        .multiline(true)
        .with_spell_language("en_US")
        .with_text_action("rewrite.clear", "Rewrite clearly")
        .with_text_action("translate", "Translate…")
        .with_initial_value(copy);
    assisted.set_focus(true);
    assisted.apply_layout_external(Rect::new(32.0, 86.0, 646.0, 164.0));
    // Prime the production glyph metrics, then select the misspelling and
    // open the same menu used by right-click / keyboard menu invocation.
    {
        let mut s = Scene::new();
        assisted.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }
    assisted.set_selection(12, 15);
    assisted.open_text_menu_at([386.0, 118.0]);

    let mut scene3 = Scene::new();
    let card = RoundedRect::from_rect(Rect::new(18.0, 18.0, 662.0, 432.0), 14.0);
    scene3.fill(
        vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.raised,
        None,
        &card,
    );
    label(
        &mut scene3,
        &mut tr,
        &theme,
        "Writing assistance",
        32.0,
        34.0,
    );
    tr.draw_text(
        &mut scene3,
        "English (US)  ·  Local  ·  Private",
        12.0,
        theme.text_dim,
        Affine::translate((32.0, 57.0)),
        None,
    );
    assisted.draw(&mut scene3, Affine::IDENTITY, &mut tr, &theme, &presence);
    Widget::draw_overlay(
        &mut assisted,
        &mut scene3,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &presence,
    );
    let mut r3 = OffscreenRenderer::new(680, 450);
    let out_assisted = dir.join("writing-assistance.png");
    r3.render_png(&scene3, theme.tokens.colors.surfaces.content, &out_assisted);
    println!("wrote {}", out_assisted.display());

    // ---- 4. Structured inputs inherit opt-out from semantic intent -------
    let specs = [
        ("Search", TextInputKind::Search, "kubernetez deployment"),
        (
            "Web address",
            TextInputKind::Url,
            "https://qwordz.example/docs",
        ),
        ("File path", TextInputKind::Path, "/home/alice/qwordz.txt"),
    ];
    let mut fields: Vec<_> = specs
        .iter()
        .map(|(_, kind, value)| {
            TextInput::local()
                .with_kind(*kind)
                .with_initial_value(value)
        })
        .collect();
    if let Some(first) = fields.first_mut() {
        first.set_focus(true);
    }
    let mut scene4 = Scene::new();
    let card = RoundedRect::from_rect(Rect::new(18.0, 18.0, 662.0, 318.0), 14.0);
    scene4.fill(
        vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.raised,
        None,
        &card,
    );
    label(
        &mut scene4,
        &mut tr,
        &theme,
        "Only natural-language prose is checked",
        32.0,
        34.0,
    );
    tr.draw_text(
        &mut scene4,
        "Search, address, path, code, number and secret fields opt out automatically.",
        12.0,
        theme.text_dim,
        Affine::translate((32.0, 57.0)),
        None,
    );
    for (index, ((caption, _, _), input)) in specs.iter().zip(fields.iter_mut()).enumerate() {
        let y = 88.0 + f64::from(u32::try_from(index).unwrap_or(u32::MAX)) * 72.0;
        label(&mut scene4, &mut tr, &theme, caption, 32.0, y);
        input.apply_layout_external(Rect::new(180.0, y - 12.0, 646.0, y + 30.0));
        input.draw(&mut scene4, Affine::IDENTITY, &mut tr, &theme, &presence);
    }
    let mut r4 = OffscreenRenderer::new(680, 336);
    let out_structured = dir.join("structured-fields.png");
    r4.render_png(
        &scene4,
        theme.tokens.colors.surfaces.content,
        &out_structured,
    );
    println!("wrote {}", out_structured.display());

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
    assert!(
        fields
            .iter()
            .all(|input| input.kind() != TextInputKind::Prose)
    );
    println!("OK: multiline, secrets, writing assistance, and semantic opt-out rendered");
}
