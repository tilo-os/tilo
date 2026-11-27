//! Write the session's package where `apps/tl/session` commits it: the compiled `surface.json`
//! and the authored `surface.toml` a compiler reads, both from the one description of what the
//! session publishes. A test holds the committed pair to it; this writes the pair when the API
//! moved.
//!
//! `cargo run -p tilo-session --example session_surface -- apps/tl/session`

fn main() -> std::io::Result<()> {
    let directory = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "apps/tl/session".to_owned());
    let directory = std::path::Path::new(&directory);
    let written = [
        ("surface.json", tilo_session::session_package::description()),
        ("surface.toml", tilo_session::session_package::authored()),
    ];
    for (name, text) in written {
        let path = directory.join(name);
        std::fs::write(&path, text)?;
        println!("Wrote {}", path.display());
    }
    Ok(())
}
