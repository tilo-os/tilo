//! End-to-end pairing check using `bluetoothd`, the pairing agent, and the
//! prompt UI.
//!
//! ```text
//! cargo run -p shell --example pairing_e2e
//! ```
//!
//! Walks the whole chain the way a pairing does:
//!
//! 1. The compositor's [`PairingService`] connects to `BlueZ` and registers the
//!    agent.
//! 2. A **root** process invokes `org.bluez.Agent1.RequestConfirmation`, which
//!    is the only thing the bus policy permits to drive an agent, and is
//!    byte-for-byte the call `bluetoothd` makes. The tool re-executes itself
//!    under `sudo` to play that part.
//! 3. The question reaches the prompt handle, is rendered by
//!    `PairingPromptView`, and the PNG is written out.
//! 4. The answer is delivered as a click on the dialog's primary action, and
//!    travels back to the caller.
//!
//! This is a tool rather than a test because it requires Bluetooth hardware;
//! every step that cannot observe its subject exits non-zero.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use shell::bluetooth::{PairingPromptHandle, PairingService};
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

const DEV: &str = "/org/bluez/hci0/dev_88_C9_E8_6F_B4_8B";
const PASSKEY: u32 = 424_242;

#[tokio::main]
async fn main() {
    if let Some(target) = arg("--as-bluez") {
        std::process::exit(match bluez_side(&target).await {
            Ok(line) => {
                println!("{line}");
                0
            }
            Err(e) => {
                eprintln!("RESULT=FAIL {e}");
                1
            }
        });
    }
    if let Err(e) = run().await {
        eprintln!("\nFAIL: {e}");
        std::process::exit(1);
    }
    println!("\nOK");
}

fn arg(flag: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == flag)
        .and_then(|i| a.get(i.saturating_add(1)).cloned())
}

/// The root half — stands in for `bluetoothd`.
async fn bluez_side(unique: &str) -> Result<String, String> {
    let conn = zbus::Connection::system()
        .await
        .map_err(|e| e.to_string())?;
    let proxy = zbus::Proxy::new(&conn, unique, tilo_bluez::AGENT_PATH, "org.bluez.Agent1")
        .await
        .map_err(|e| e.to_string())?;
    let dev = zbus::zvariant::ObjectPath::try_from(DEV).map_err(|e| e.to_string())?;
    match proxy
        .call_method("RequestConfirmation", &(&dev, PASSKEY))
        .await
    {
        Ok(_) => Ok("RESULT=OK".to_string()),
        Err(zbus::Error::MethodError(name, d, _)) => Ok(format!(
            "RESULT=ERROR name={} detail={}",
            name.as_str(),
            d.unwrap_or_default()
        )),
        Err(e) => Err(e.to_string()),
    }
}

// Not `Send`: `PairingPromptView` holds a `Box<dyn Widget>`, and `Widget`
// is not `Send` (widgets are single-thread UI state). This binary only
// ever `.await`s `run()` directly from `#[tokio::main]`'s `main`, never
// spawns it as a task, so the runtime never needs the future to be `Send`.
#[allow(
    clippy::future_not_send,
    reason = "Box<dyn Widget> is !Send by design; run() is only ever awaited in-place, never spawned"
)]
async fn run() -> Result<(), String> {
    // 1. The compositor's service.
    let handle = PairingPromptHandle::new();
    let service = PairingService::start(handle.clone())
        .await
        .map_err(|e| format!("pairing service did not start: {e}"))?;
    let unique = service
        .client()
        .connection()
        .unique_name()
        .ok_or("no unique name on the agent connection")?
        .to_string();
    println!("service  agent registered, connection {unique}");
    if handle.is_prompting() {
        return Err("a prompt existed before anything asked".into());
    }

    // 2. bluetoothd's part, from root.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let unique_for_child = unique.clone();
    let caller = tokio::task::spawn_blocking(move || {
        std::process::Command::new("sudo")
            .args(["-n", &exe.to_string_lossy()])
            .args(["--as-bluez", &unique_for_child])
            .output()
    });

    // 3. Wait for the question to arrive.
    let mut request = None;
    for _ in 0..100 {
        if let Some(r) = handle.current() {
            request = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let request = request.ok_or(
        "no pairing question reached the compositor — check that sudo is available, since \
         only root may call org.bluez.Agent1",
    )?;
    println!(
        "prompt   \"{}\" asks {:?}",
        request.device_name, request.prompt
    );
    if request.prompt != (tilo_bluez::PairingPrompt::ConfirmPasskey { passkey: PASSKEY }) {
        return Err(format!("unexpected prompt: {:?}", request.prompt));
    }

    // 4. Render the widget for the request.
    let out = PathBuf::from(
        std::env::var("PAIRING_E2E_OUT").unwrap_or_else(|_| "target/pairing-e2e.png".to_string()),
    );
    let mut view = PairingPromptView::new(request.clone());
    render(&mut view, &out)?;
    println!("render   wrote {}", out.display());

    // 5. Answer as a click on the primary action would.
    let state = State::new();
    view.press_accept();
    view.sync(&state.doc, &state.root);
    let reply = view
        .take_answer()
        .ok_or("clicking the primary action produced no answer")?;
    println!("answer   widget produced {reply:?}");
    if !handle.answer(reply) {
        return Err("the handle had nothing to answer".into());
    }
    if handle.is_prompting() {
        return Err("the prompt was still up after being answered".into());
    }

    // 6. The caller must have been unblocked with success.
    let out = caller
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("could not re-exec under sudo: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let line = text
        .lines()
        .find(|l| l.starts_with("RESULT="))
        .ok_or_else(|| format!("no result from the root half: {}", text.trim()))?;
    println!("bluez    {line}");
    if line != "RESULT=OK" {
        return Err(format!("the accepted pairing did not succeed: {line}"));
    }

    service.shutdown().await;
    println!("service  agent unregistered");
    Ok(())
}

fn render(view: &mut PairingPromptView, out: &PathBuf) -> Result<(), String> {
    const W: u32 = 720;
    const H: u32 = 480;
    // `as f32` on compile-time canvas-size literals: 720/480 are exactly
    // representable, well under f32's 24-bit mantissa headroom.
    #[allow(
        clippy::cast_precision_loss,
        reason = "W/H are small compile-time constants, exactly representable in f32"
    )]
    const W_F32: f32 = W as f32;
    #[allow(
        clippy::cast_precision_loss,
        reason = "W/H are small compile-time constants, exactly representable in f32"
    )]
    const H_F32: f32 = H as f32;
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = view.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(W_F32),
                height: AvailableSpace::Definite(H_F32),
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut tr)
                })
            },
        )
        .map_err(|e| e.to_string())?;
    view.apply_layout(&taffy, node, Point::ZERO);
    let mut scene = Scene::new();
    view.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::default(),
    );
    OffscreenRenderer::new(W, H).render_png(&scene, theme.tokens.colors.surfaces.base, out);
    Ok(())
}
