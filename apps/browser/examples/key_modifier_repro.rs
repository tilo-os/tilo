//! Prove keyboard modifiers reach web content. Loads a page with a
//! textarea prefilled with "hello world " (caret at end) that recolours
//! itself by its own value, then sends Backspace with Ctrl held:
//!
//! - Ctrl+Backspace in a textarea = delete word backward → "hello "
//!   → the textarea turns GREEN (modifier was delivered).
//! - Plain Backspace deletes one char → "hello world" → RED (modifier
//!   was dropped — the old `modifiers: 0` bug).
//!
//! Run:
//!   WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 \
//!   cargo run -p browser --example key_modifier_repro --features wpe --release

use std::sync::Arc;
use ui::time::Duration;

use ui::webview::wpe::WpeEngine;
use ui::webview::{KeyInput, KeyModifiers, NamedKey, SharedEngine, WebViewCommand, WebViewEngine};

const PAGE: &str = r#"<!doctype html><html><body style="margin:0">
<textarea id="t" style="position:fixed;inset:0;width:100vw;height:100vh;border:0;font-size:40px"></textarea>
<script>
const t = document.getElementById('t');
t.value = "hello world ";
t.focus();
t.setSelectionRange(t.value.length, t.value.length);
function paint(){
  const v = t.value;
  let c = '#ffffff';
  if (v === 'hello ') c = '#00c000';           // ctrl+backspace: word deleted
  else if (v === 'hello world') c = '#c00000'; // plain backspace: one char
  else if (v !== 'hello world ') c = '#c08000'; // something else
  t.style.background = c;
}
setInterval(paint, 40);
paint();
</script></body></html>"#;

fn latest(engine: &dyn WebViewEngine) -> Option<(Arc<Vec<u8>>, u32, u32)> {
    engine
        .frames()
        .lock()
        .unwrap()
        .as_ref()
        .map(|f| (f.rgba.clone(), f.width, f.height))
}

/// Count pixels near green / red / white and return the dominant label.
fn dominant(rgba: &[u8]) -> (&'static str, [u64; 3]) {
    let (mut green, mut red, mut white) = (0u64, 0u64, 0u64);
    for px in rgba.chunks_exact(4) {
        let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
        if g > 140 && r < 120 && b < 120 {
            green += 1;
        } else if r > 140 && g < 120 && b < 120 {
            red += 1;
        } else if r > 200 && g > 200 && b > 200 {
            white += 1;
        }
    }
    let counts = [green, red, white];
    let label = if green >= red && green > white / 4 {
        "GREEN (ctrl+backspace word-delete — PASS)"
    } else if red >= green && red > 0 {
        "RED (plain backspace — modifier dropped, FAIL)"
    } else {
        "neither (indeterminate)"
    };
    (label, counts)
}

fn main() {
    let engine: SharedEngine = WpeEngine::new().expect("engine");
    engine.send(WebViewCommand::Resize {
        width: 800,
        height: 400,
        scale_factor: 1.0,
    });
    let url = format!("data:text/html,{}", urlencode(PAGE));
    engine.send(WebViewCommand::Navigate(url));
    engine.send(WebViewCommand::SetFocused(true));

    // Wait for the textarea (white) to render.
    let deadline = ui::time::now() + Duration::from_secs(10);
    while ui::time::now() < deadline {
        if let Some((r, _, _)) = latest(&*engine) {
            let (_, [_, _, white]) = dominant(&r);
            if white > 1000 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(400));
    let before = latest(&*engine).map(|(r, _, _)| dominant(&r)).unwrap();
    println!(
        "before keypress: {}  counts(g,r,w)={:?}",
        before.0, before.1
    );

    // Ctrl+Backspace (down then up).
    let mods = KeyModifiers {
        ctrl: true,
        ..Default::default()
    };
    engine.send(WebViewCommand::Key {
        key: KeyInput::Named(NamedKey::Backspace),
        down: true,
        modifiers: mods,
    });
    engine.send(WebViewCommand::Key {
        key: KeyInput::Named(NamedKey::Backspace),
        down: false,
        modifiers: mods,
    });

    std::thread::sleep(Duration::from_millis(600));
    let after = latest(&*engine).map(|(r, _, _)| dominant(&r)).unwrap();
    println!(
        "after ctrl+backspace: {}  counts(g,r,w)={:?}",
        after.0, after.1
    );
}

/// Minimal percent-encoding for the data: URL body.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
