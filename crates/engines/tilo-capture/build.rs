//! Which backend serves each capability. The features select a native
//! backend (`GStreamer`, or `AVFoundation` on macOS); code waits for these
//! cfgs.

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let mac = os == "macos";
    let gst = !mac;
    let feature = |name: &str| std::env::var_os(format!("CARGO_FEATURE_{name}")).is_some();
    let cfgs = [
        ("native_camera", feature("CAMERA")),
        ("native_microphone", feature("MICROPHONE")),
        ("native_screen", feature("SCREEN")),
        ("gst_camera", gst && feature("CAMERA")),
        ("gst_microphone", gst && feature("MICROPHONE")),
        ("gst_screen", gst && feature("SCREEN")),
        ("avf_camera", mac && feature("CAMERA")),
        ("avf_microphone", mac && feature("MICROPHONE")),
        ("avf_screen", mac && feature("SCREEN")),
        ("portal_screen", os == "linux" && feature("SCREEN")),
        ("sane_scanner", os == "linux" && feature("SCANNER")),
        (
            "codes",
            !matches!(
                std::env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
                Ok("wasm32")
            ) && feature("CODES"),
        ),
    ];
    for (cfg, on) in cfgs {
        println!("cargo::rustc-check-cfg=cfg({cfg})");
        if on {
            println!("cargo::rustc-cfg={cfg}");
        }
    }
    let any = |prefix: &str| cfgs.iter().any(|(cfg, on)| *on && cfg.starts_with(prefix));
    for backend in ["gst", "avf"] {
        println!("cargo::rustc-check-cfg=cfg({backend})");
        if any(&format!("{backend}_")) {
            println!("cargo::rustc-cfg={backend}");
        }
    }
}
