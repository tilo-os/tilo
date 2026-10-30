//! `cargo run --release -p tilolang-app --example seeded_workspace -- <package dir> <items> <out dir>`:
//! a profile whose workspace holds a package's record seeded with `<items>` items, and the package's
//! bundle, for measuring the real window's first frame on it:
//!
//! ```text
//! TILO_APP_MODULE=<out>/bundle TILO_STARTUP_METRICS_PATH=<out>/metrics \
//!   tilolang-app --first-frame --data-dir <out>/data
//! ```
//!
//! The workspace is the one `--data-dir` opens (`personal`), and the record is installed as the workspace's
//! own document, as an earlier session of the app left it. With `--index`, the workspace's index is
//! kept beside it too, as that session's window kept it.

use std::path::PathBuf;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (index, args) = match args.split_first() {
        Some((flag, rest)) if flag == "--index" => (true, rest),
        _ => (false, args.as_slice()),
    };
    let [package, items, out] = args else {
        return Err("usage: seeded_workspace [--index] <package dir> <items> <out dir>".into());
    };
    let items: usize = items
        .parse()
        .map_err(|_| format!("`{items}` is no count"))?;
    let out = PathBuf::from(out);
    let packages = tilolang_tilo::bench::compile(&PathBuf::from(package))?;
    let main = packages.last().ok_or("no package")?;
    let imports = packages
        .iter()
        .filter(|compiled| compiled.meta.app != main.meta.app);
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
        other => return Err(format!("the workspace's record did not land: {other:?}")),
    }
    if index {
        keep_index(&packages, &doc, &data)?;
    }
    println!("{} with {items} items in {}", main.meta.app, data.display());
    Ok(())
}

/// Keep the index of the workspace `doc` is the record of where the window `--data-dir data` opens
/// reads it.
fn keep_index(
    packages: &[tilolang_tilo::tilolang::Compiled],
    doc: &ui::automerge::AutoCommit,
    data: &std::path::Path,
) -> Result<(), String> {
    let main = packages.last().ok_or("no package")?;
    let files = ui::storage::AppFiles::at(ui::storage::AppPaths::under(
        data.join("files"),
        &main.meta.app,
    ));
    let base = tilolang_app::app::indexes(&files).ok_or("the profile keeps no cache")?;
    let host = tilolang_tilo::host::Host::open(
        tilolang_tilo::host::Opening {
            record: Some(doc),
            index_dir: Some(tilolang_tilo::index::room_dir(&base, "personal")),
            device: None,
        },
        tilolang_tilo::runtime::Principal::new("seed"),
        tilolang_tilo::authority::holding_nothing("seed"),
    )
    .map_err(|e| e.to_string())?;
    for compiled in packages {
        host.load_compiled(compiled).map_err(|e| e.to_string())?;
    }
    host.wait_index();
    if !host.index_ready() {
        return Err("the index was not built".into());
    }
    host.persist_index();
    Ok(())
}
