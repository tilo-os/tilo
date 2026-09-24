//! Print what the engine extracts from a PDF page and what the text layer
//! makes of it: `dump_text FILE [PAGE]`.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a developer tool"
)]

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: dump_text FILE [PAGE]");
        std::process::exit(2);
    };
    let page: usize = args.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let run = || -> tilo_pdf::Result<()> {
        let engine = tilo_pdf::Engine::new()?;
        let doc = engine.open_path(std::path::Path::new(&path))?;
        println!(
            "pages: {}  sandboxed: {}",
            doc.page_count(),
            engine.is_sandboxed()
        );
        for c in doc.raw_text(page)? {
            let ch = char::from_u32(c.unicode).unwrap_or('\u{FFFD}');
            println!(
                "{:4} U+{:04X} {:?} flags={} loose=({:.1},{:.1})-({:.1},{:.1})",
                c.index, c.unicode, ch, c.flags, c.loose.x0, c.loose.y0, c.loose.x1, c.loose.y1
            );
        }
        let layer = doc.text(page)?;
        println!(
            "--- layer ({} lines, {} blocks) ---",
            layer.lines().len(),
            layer.blocks().len()
        );
        println!("{}", layer.text());
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
