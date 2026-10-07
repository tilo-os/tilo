include!("fixture_build.rs");

use cfg_aliases::cfg_aliases;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    emit_hex_fixture(include_str!("examples/data/icon.png.hex"), "icon.png")?;
    emit_hex_fixture(include_str!("examples/data/cross.png.hex"), "cross.png")?;
    emit_hex_fixture(include_str!("examples/data/cross2.png.hex"), "cross2.png")?;
    emit_hex_fixture(
        include_str!("examples/data/gradient.png.hex"),
        "gradient.png",
    )?;

    // The script doesn't depend on our code.
    println!("cargo:rerun-if-changed=build.rs");

    // Setup cfg aliases.
    cfg_aliases! {
        // Systems.
        android_platform: { target_os = "android" },
        web_platform: { all(target_family = "wasm", target_os = "unknown") },
        macos_platform: { target_os = "macos" },
        ios_platform: { target_os = "ios" },
        windows_platform: { target_os = "windows" },
        apple: { any(target_os = "ios", target_os = "macos") },
        free_unix: { all(unix, not(apple), not(android_platform), not(target_os = "emscripten")) },
        redox: { target_os = "redox" },

        // Native displays.
        x11_platform: { all(feature = "x11", free_unix, not(redox)) },
        wayland_platform: { all(feature = "wayland", free_unix, not(redox)) },
        orbital_platform: { redox },
    }

    // Winit defined cfgs.
    println!("cargo:rustc-check-cfg=cfg(unreleased_changelogs)");
    Ok(())
}
