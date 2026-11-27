//! The packages built into this build, compiled as the build is: a device reads each one's module
//! and ids lock, as an installed package's payload is read, and compiles none.

use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;

include!("src/records/compile.rs");

/// The packages built into this build, by their directory under `apps/tl` and their files, each
/// app after those it uses. A device's daemon arms the alarms set for it from Clocks' record.
const BUILT_IN: &[&[(&str, &[&str])]] = &[
    &[("chat", &["app.tl"])],
    &[("discord", &["app.tl"])],
    &[("clocks", &["app.tl"])],
    &[
        ("discussion", &["app.tl"]),
        ("notes", &["app.tl"]),
        ("contacts", &["app.tl", "identities.tl"]),
        ("tasks", &["app.tl"]),
    ],
];

fn main() -> Result<(), Box<dyn Error>> {
    let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    let out = Path::new(&std::env::var("OUT_DIR")?).to_owned();
    let platform = tilolang_tilo::platform::platform_packages()?;
    let read = |app: &str, file: &str| {
        println!("cargo::rerun-if-changed=../../../apps/tl/{app}/{file}");
        std::fs::read_to_string(apps.join(app).join(file))
    };
    let mut built = String::from("&[");
    for group in BUILT_IN {
        let mut sources = Vec::new();
        for (app, files) in *group {
            let source = files.iter().map(|file| read(app, file));
            sources.push((
                source.collect::<Result<Vec<_>, _>>()?.join("\n"),
                read(app, "app.ids")?,
            ));
        }
        let sources: Vec<_> = sources
            .iter()
            .map(|(tl, ids)| (tl.as_str(), ids.as_str()))
            .collect();
        built.push_str("&[");
        for ((app, _), package) in group.iter().zip(compile_each(&sources, &platform)?) {
            let (wasm, ids) = (
                out.join(format!("{app}.wasm")),
                out.join(format!("{app}.ids")),
            );
            std::fs::write(&wasm, &package.wasm)?;
            std::fs::write(&ids, package.ids.to_text())?;
            let out = "concat!(env!(\"OUT_DIR\"), \"/";
            write!(
                built,
                "(include_bytes!({out}{app}.wasm\")), include_str!({out}{app}.ids\"))),"
            )?;
        }
        built.push_str("],");
    }
    built.push(']');
    Ok(std::fs::write(out.join("built_in.rs"), built)?)
}
