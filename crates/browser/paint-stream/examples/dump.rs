//! Print the layers and items a snapshot converts to.
//!
//! `cargo run -p tilo-paint-stream --example dump -- SNAPSHOT.json [WIDTH HEIGHT]`
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: dump SNAPSHOT.json [WIDTH HEIGHT]")?;
    let w: f32 = args.next().map_or(Ok(800.0), |s| s.parse())?;
    let h: f32 = args.next().map_or(Ok(600.0), |s| s.parse())?;
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let f = tilo_paint_stream::snapshot::convert(&v, tilo_paint_stream::Point { x: w, y: h }, 1)?;
    for (id, l) in &f.layers {
        println!(
            "layer {id} parent {} {:?} origin {:?} size {:?} clips {} scroll {:?} opacity {} items {}",
            l.parent,
            l.anchor,
            l.origin,
            l.size,
            l.clips,
            l.scroll,
            l.opacity,
            l.items.len()
        );
        for i in &l.items {
            println!("   {i:?}");
        }
    }
    Ok(())
}
