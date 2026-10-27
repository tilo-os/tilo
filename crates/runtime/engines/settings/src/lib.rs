//! Settings' core and the panes people rarely open, and the libraries only
//! Settings uses, as an engine library beside the shared runtime
//! (`libtilo_settings_engine.so`).
//!
//! Settings' core (`settings-core`: the window, its effect vocabulary, its
//! resource and what every pane shares) and the panes people rarely open
//! (`settings-rare`: accounts, agent models, devices and sync, speakers,
//! storage and drives, disk unlock, printers, users, region, updates and
//! about) are here; the app's package carries the panes people open often,
//! which register with the core beside the rare ones.
//!
//! Settings administers the model providers (`provider-settings-ui`), mounts
//! NTFS drives through the root service (`tilo-ntfs-mount`'s protocol),
//! offers the bundled wallpapers (`tilo-wallpapers`, whose catalogue is TOML),
//! edits the accessibility settings (`tilo-accessibility-settings`) and
//! draws the keyboard with its layout's legends over the PC models' outlines
//! (`tilo-pc-keyboard-geometry`, 130 KB of JSON), and chooses the time zone
//! on a globe (`tilo-zone-globe`, with the world's coastlines). No
//! other app does, so none of them belongs in the runtime every app maps;
//! here they are a library of their own in the runtime bundle that only
//! Settings names `DT_NEEDED`, beside the accounts engine, and the tapp
//! carries Settings' own code.
//!
//! Like `runtime-dylib`, this crate re-exports the crates it provides, which
//! records them as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
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
pub use settings_core;
#[cfg(feature = "thin")]
pub use settings_rare;
#[cfg(feature = "thin")]
pub use tilo_accessibility_settings;
#[cfg(feature = "thin")]
pub use tilo_wallpapers;
#[cfg(feature = "thin")]
pub use zone_globe;

/// Run the engine's own code: read the wallpaper catalogue, which must name a
/// default among its wallpapers, read the keyboard outlines, and ask the NTFS
/// mount service at a socket that does not exist, which must fail rather
/// than answer.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
///
/// # Errors
/// Names what was read or answered wrongly.
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
