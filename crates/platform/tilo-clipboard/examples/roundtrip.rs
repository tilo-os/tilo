//! Copy text to this platform's clipboard and read it back.
//!
//! `cargo run -p tilo-clipboard --example roundtrip -- [TEXT]`
//!
//! Prints the backend, what was read back, and whether it matched; another
//! process (`pbpaste`, `wl-paste`) sees the same text afterwards.
use tilo_clipboard::ClipboardTarget;

fn main() {
    let text = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tilo-clipboard round trip".into());
    let clipboard = tilo_clipboard::system();
    println!("backend: {}", clipboard.name());
    println!(
        "set: {}",
        clipboard.set_text(ClipboardTarget::Clipboard, &text)
    );
    let read = clipboard.text(ClipboardTarget::Clipboard);
    println!("read: {read:?}");
    println!("match: {}", read.as_deref() == Some(text.as_str()));
}
