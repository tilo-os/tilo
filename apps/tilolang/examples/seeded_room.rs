//! `cargo run --release -p tilolang-app --example seeded_room -- <package dir> <items> <out dir>`:
//! a profile whose room holds a package's record seeded with `<items>` items, and the package's
//! bundle, for measuring the real window's first frame on it:
//!
//! ```text
//! TILO_APP_MODULE=<out>/bundle TILO_STARTUP_METRICS_PATH=<out>/metrics \
//!   tilolang-app --first-frame --data-dir <out>/data
//! ```
//!
//! The room is the one `--data-dir` opens (`personal`), and the record is installed as the room's
//! own document, as an earlier session of the app left it.

use std::path::PathBuf;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [package, items, out] = args.as_slice() else {
        return Err("usage: seeded_room <package dir> <items> <out dir>".into());
    };
    let items: usize = items
        .parse()
        .map_err(|_| format!("`{items}` is no count"))?;
    let out = PathBuf::from(out);
    let packages = tilolang_tilo::bench::compile(&PathBuf::from(package))?;
    let main = packages.last().ok_or("no package")?;
    let imports = packages
        .iter()
        .filter(|compiled| compiled.meta.app != main.meta.app)
        .map(|compiled| compiled.meta.clone())
        .collect();
    tilolang_tilo::bundle::Bundle::new(main, imports)
        .write(&out.join("bundle"))
        .map_err(|e| e.to_string())?;
    let mut doc = tilolang_tilo::bench::seeded_record(&packages, items)?;
    let data = std::path::absolute(out.join("data")).map_err(|e| e.to_string())?;
    let profile = ui::application::NativeProfile::open(&data, None).map_err(|e| e.to_string())?;
    let landing = ui::record::transfer::install_room_document(
        profile.store(),
        &main.meta.app,
        "personal",
        &mut doc,
    )
    .map_err(|e| e.to_string())?;
    profile.store().flush(None).map_err(|e| e.to_string())?;
    match landing.durability(&**profile.store()) {
        ui::record::transfer::Durability::Durable => {}
        other => return Err(format!("the room's record did not land: {other:?}")),
    }
    println!("{} with {items} items in {}", main.meta.app, data.display());
    Ok(())
}
