//! Render each Bluetooth pairing prompt to PNG.
//!
//! ```text
//! cargo run -p shell --example pairing_prompt_shots
//! ```
//!
//! Same widget tree, layout and draw passes the compositor uses, so this is
//! how the dialog is reviewed.

use std::collections::HashMap;
use std::path::PathBuf;

use qos_bluez::{PairingPrompt, PairingRequest};
use shell::bluetooth_prompt::PairingPromptView;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
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

fn shoot(theme: &Theme, out: PathBuf, name: &str, prompt: PairingPrompt) {
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
                width: AvailableSpace::Definite(WIDTH as f32),
                height: AvailableSpace::Definite(HEIGHT as f32),
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut tr)
                })
            },
        )
        .unwrap();
    view.apply_layout(&taffy, node, Point::ZERO);
    view.sync(&mut state.doc, &state.root);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    view.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {}", out.display());
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let dir =
        std::env::var("PAIRING_SHOTS_OUT").unwrap_or_else(|_| "target/pairing-shots".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);

    for (theme, suffix) in [(Theme::dark(), "dark"), (Theme::light(), "light")] {
        shoot(
            &theme,
            dir.join(format!("confirm-{suffix}.png")),
            "WH-1000XM4",
            PairingPrompt::ConfirmPasskey { passkey: 424_242 },
        );
        shoot(
            &theme,
            dir.join(format!("display-{suffix}.png")),
            "MX KEYS S",
            PairingPrompt::DisplayPasskey {
                passkey: 731_908,
                entered: 3,
            },
        );
        shoot(
            &theme,
            dir.join(format!("entry-{suffix}.png")),
            "Keyboard Craft",
            PairingPrompt::RequestPasskey,
        );
        shoot(
            &theme,
            dir.join(format!("just-works-{suffix}.png")),
            "Bose Mini II SoundLink",
            PairingPrompt::ConfirmPairing,
        );
    }
}
