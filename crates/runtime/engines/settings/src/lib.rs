//! Settings itself and the libraries only Settings uses, as its own library in the
//! runtime bundle (`libtilo_settings_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
#[cfg(feature = "thin")]
extern crate tilo_accounts_engine;

#[cfg(feature = "thin")]
pub use ntfs_mount;
#[cfg(feature = "thin")]
pub use pc_keyboard_geometry;
#[cfg(feature = "thin")]
pub use provider_settings_ui;
#[cfg(feature = "thin")]
pub use settings;
#[cfg(feature = "thin")]
pub use settings_sections;
#[cfg(feature = "thin")]
pub use tilo_accessibility_settings;
#[cfg(feature = "thin")]
pub use tilo_wallpapers;
#[cfg(feature = "thin")]
pub use zone_globe;

#[cfg(feature = "thin")]
ui::engine_main!(
    settings::SettingsShell,
    init = settings::enable_provider_administration
);

/// The engine's self-check ([`self_check`]) for its check binary, by the
/// unmangled name the binary links, as Settings' binary
/// links [`tilo_settings_engine_run`]: nothing compiles against this library, so
/// it exports no Rust item. Prints the report; whether it passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_settings_engine_check() -> bool {
    match self_check() {
        Ok(report) => {
            println!("tilo-settings-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-settings-engine: {error}");
            false
        }
    }
}

/// Run the engine's own code: read the wallpaper catalogue, which must name a
/// default among its wallpapers, read the keyboard outlines, and ask the NTFS
/// mount service at a socket that does not exist, which must fail rather
/// than answer.
///
/// The check binary runs it through its unmangled entry, so a release proves
/// the library loads and runs against the runtime it ships with.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    let wallpapers = tilo_wallpapers::all().len();
    let default = tilo_wallpapers::default_wallpaper()
        .ok_or_else(|| format!("the catalogue of {wallpapers} wallpapers has no default"))?;
    let keyboards = pc_keyboard_geometry::models()?.len();
    let scratch = std::env::temp_dir().join(format!(
        "tilo-settings-engine-check-{}.sock",
        std::process::id()
    ));
    let asked = ntfs_mount::protocol::call(
        &scratch,
        ntfs_mount::protocol::Request::Drives,
        std::time::Duration::from_secs(1),
    );
    if asked.is_ok() {
        return Err("a mount service that does not exist answered".to_owned());
    }
    Ok(format!(
        "read {wallpapers} wallpapers, {} by default, and {keyboards} keyboard models; no mount \
         service answered",
        default.id
    ))
}
