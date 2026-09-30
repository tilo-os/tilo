//! Which backend serves each capability. The features select a native
//! backend (`GStreamer`, or `AVFoundation` on macOS) and compile to nothing on
//! the web, where the page's media devices serve every capability; code
//! gates on these cfgs.

fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let web = arch == "wasm32";
    let mac = os == "macos";
    let gst = !web && !mac;
    let feature = |name: &str| std::env::var_os(format!("CARGO_FEATURE_{name}")).is_some();
    let cfgs = [
        ("web_page", web && os == "unknown"),
        ("native_camera", !web && feature("CAMERA")),
        ("native_microphone", !web && feature("MICROPHONE")),
        ("native_screen", !web && feature("SCREEN")),
        ("gst_camera", gst && feature("CAMERA")),
        ("gst_microphone", gst && feature("MICROPHONE")),
        ("gst_screen", gst && feature("SCREEN")),
        ("avf_camera", mac && feature("CAMERA")),
        ("avf_microphone", mac && feature("MICROPHONE")),
        ("avf_screen", mac && feature("SCREEN")),
        ("portal_screen", os == "linux" && feature("SCREEN")),
        ("sane_scanner", os == "linux" && feature("SCANNER")),
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
