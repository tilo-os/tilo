//! Render each Bluetooth pairing prompt to PNG.
//!
//! ```text
//! cargo run -p shell --example pairing_prompt_shots
//! ```
//!
//! Same widget tree, layout and draw passes the compositor uses, so this is
//! how the dialog is reviewed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shell::bluetooth_prompt::PairingPromptView;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_bluez::{PairingPrompt, PairingRequest};
use ui::State;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

const WIDTH: u32 = 720;
const HEIGHT: u32 = 480;
// `as f32` on compile-time canvas-size literals: 720/480 are exactly
// representable, well under f32's 24-bit mantissa headroom.
#[allow(
    clippy::cast_precision_loss,
    reason = "WIDTH/HEIGHT are small compile-time constants, exactly representable in f32"
)]
const WIDTH_F32: f32 = WIDTH as f32;
#[allow(
    clippy::cast_precision_loss,
    reason = "WIDTH/HEIGHT are small compile-time constants, exactly representable in f32"
)]
const HEIGHT_F32: f32 = HEIGHT as f32;

fn shoot(theme: &Theme, out: &Path, name: &str, prompt: PairingPrompt) -> Result<(), String> {
    let mut state = State::new();
    let mut view = PairingPromptView::new(PairingRequest {
        device_path: "/org/bluez/hci0/dev_88_C9_E8_6F_B4_8B".into(),
        device_name: name.to_string(),
        prompt,
    });
    view.sync(&mut state.doc, &state.root);

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = view.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(WIDTH_F32),
                height: AvailableSpace::Definite(HEIGHT_F32),
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut tr)
                })
            },
        )
        .map_err(|e| format!("compute_layout_with_measure: {e}"))?;
    view.apply_layout(&taffy, node, Point::ZERO);
    view.sync(&mut state.doc, &state.root);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    view.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.tokens.colors.surfaces.base, out);
    println!("wrote {}", out.display());
    Ok(())
}

fn main() -> Result<(), String> {
    ui::i18n::I18nManager::init("en-US");
    let dir =
        std::env::var("PAIRING_SHOTS_OUT").unwrap_or_else(|_| "target/pairing-shots".to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all({dir}): {e}"))?;
    let dir = PathBuf::from(dir);

    for (theme, suffix) in [(Theme::dark(), "dark"), (Theme::light(), "light")] {
        shoot(
            &theme,
            &dir.join(format!("confirm-{suffix}.png")),
            "WH-1000XM4",
            PairingPrompt::ConfirmPasskey { passkey: 424_242 },
        )?;
        shoot(
            &theme,
            &dir.join(format!("display-{suffix}.png")),
            "MX KEYS S",
            PairingPrompt::DisplayPasskey {
                passkey: 731_908,
                entered: 3,
            },
        )?;
        shoot(
            &theme,
            &dir.join(format!("entry-{suffix}.png")),
            "Keyboard Craft",
            PairingPrompt::RequestPasskey,
        )?;
        shoot(
            &theme,
            &dir.join(format!("just-works-{suffix}.png")),
            "Bose Mini II SoundLink",
            PairingPrompt::ConfirmPairing,
        )?;
    }
    Ok(())
}
